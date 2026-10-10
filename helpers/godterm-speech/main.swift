// godterm-speech: Apple's on-device speech recognition (SpeechAnalyzer +
// SpeechTranscriber, macOS 26+) for GodTerm.
//
//   godterm-speech --file utt.wav [--locale en-US] [--context "GodTerm,account two"]
//   godterm-speech --serve          one JSON request per stdin line:
//                                   {"wav": "/path.wav", "context": ["..."]}
//   godterm-speech --check          is it available, is the model installed
//
// Mic capture with Apple voice processing (echo cancellation, noise
// suppression, gain control, and the system Voice Isolation mic mode):
//
//   godterm-speech --mic [--no-vp]  16 kHz mono s16le PCM on stdout; JSON
//                                   status lines on stderr:
//                                   {"event":"started","vp":true,"mode":"standard",...}
//                                   {"event":"mode","mode":"voice_isolation"}
//                                   {"event":"error","error":"..."}
//                                   {"event":"input_mute","muted":true}  the headset mute gesture
//                                   {"event":"route","what":"..."}  route change (engine restarted)
//                                   {"event":"retry"|"path"|"note"|"remote",...}  diagnostics
//                                   Voice processing that cannot start (-10875 with a
//                                   Bluetooth headset) falls back to plain capture here.
//                                   --remote: also claim the remote commands (tap-test only)
//   godterm-speech --mic-file x.wav [--realtime]
//                                   the same conversion and framing from a file
//   godterm-speech --mic-check      {"mic": true, "mode": "...", "preferred": "..."}
//   godterm-speech --mic-modes      open the system mic mode picker
//
// GODTERM_NO_MIC set: --mic refuses (exit 77) and never opens the mic.
//
// Each result is one JSON line on stdout:
//   {"text": "...", "alternatives": ["..."], "confidence": 0.93, "ms": 210}
// Audio never leaves the Mac.

import AVFoundation
import Foundation
import MediaPlayer
import Speech

struct Out: Encodable {
    var text: String
    var alternatives: [String]
    var confidence: Double?
    var ms: Int
    var error: String?
}

func emit<T: Encodable>(_ v: T) {
    let enc = JSONEncoder()
    if let d = try? enc.encode(v), let s = String(data: d, encoding: .utf8) {
        print(s)
        fflush(stdout)
    }
}

func locale(_ id: String) async -> Locale? {
    await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: id))
}

/// Make sure the on-device model for the locale is installed (downloads it
/// once, on device, through the system).
func ensureModel(_ t: SpeechTranscriber) async throws {
    if let req = try await AssetInventory.assetInstallationRequest(supporting: [t]) {
        try await req.downloadAndInstall()
    }
}

func transcribe(path: String, loc: Locale, context: [String]) async -> Out {
    let t0 = Date()
    do {
        let t = SpeechTranscriber(locale: loc, transcriptionOptions: [], reportingOptions: [.alternativeTranscriptions], attributeOptions: [.transcriptionConfidence])
        try await ensureModel(t)
        let file = try AVAudioFile(forReading: URL(fileURLWithPath: path))
        let analyzer = SpeechAnalyzer(modules: [t])
        if !context.isEmpty {
            let ctx = AnalysisContext()
            ctx.contextualStrings[.general] = context
            try await analyzer.setContext(ctx)
        }
        var text = ""
        var alts: [String] = []
        var confs: [Double] = []
        let reader = Task {
            for try await r in t.results where r.isFinal {
                let s = String(r.text.characters)
                text += (text.isEmpty ? "" : " ") + s.trimmingCharacters(in: .whitespaces)
                if alts.isEmpty {
                    alts = r.alternatives.map { String($0.characters) }
                }
                for run in r.text.runs {
                    if let c = run.transcriptionConfidence {
                        confs.append(c)
                    }
                }
            }
        }
        if let last = try await analyzer.analyzeSequence(from: file) {
            try await analyzer.finalizeAndFinish(through: last)
        } else {
            await analyzer.cancelAndFinishNow()
        }
        _ = try await reader.value
        let conf = confs.isEmpty ? nil : confs.reduce(0, +) / Double(confs.count)
        return Out(text: text.trimmingCharacters(in: .whitespaces), alternatives: alts, confidence: conf, ms: Int(Date().timeIntervalSince(t0) * 1000), error: nil)
    } catch {
        return Out(text: "", alternatives: [], confidence: nil, ms: Int(Date().timeIntervalSince(t0) * 1000), error: "\(error)")
    }
}

struct Req: Decodable {
    var wav: String
    var context: [String]?
}


// MARK: mic capture

func err(_ s: String) -> NSError {
    NSError(domain: "godterm-speech", code: 1, userInfo: [NSLocalizedDescriptionKey: s])
}

func status(_ d: [String: Any]) {
    if let j = try? JSONSerialization.data(withJSONObject: d, options: [.sortedKeys]),
       let s = String(data: j, encoding: .utf8) {
        FileHandle.standardError.write((s + "\n").data(using: .utf8)!)
    }
}

func modeName(_ m: AVCaptureDevice.MicrophoneMode) -> String {
    switch m {
    case .standard: return "standard"
    case .wideSpectrum: return "wide_spectrum"
    case .voiceIsolation: return "voice_isolation"
    @unknown default: return "unknown"
    }
}

func micModes() -> [String: Any] {
    ["mode": modeName(AVCaptureDevice.activeMicrophoneMode),
     "preferred": modeName(AVCaptureDevice.preferredMicrophoneMode)]
}

/// Mono float at the input rate -> 16 kHz mono s16le on stdout.
final class Pipe16k {
    let outFmt = AVAudioFormat(commonFormat: .pcmFormatInt16, sampleRate: 16000, channels: 1, interleaved: true)!
    let monoFmt: AVAudioFormat
    let conv: AVAudioConverter
    var bytes = 0

    init(rate: Double) throws {
        guard let m = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: rate, channels: 1, interleaved: false),
              let c = AVAudioConverter(from: m, to: outFmt) else { throw err("no converter for \(rate) Hz") }
        monoFmt = m
        conv = c
        conv.sampleRateConverterQuality = AVAudioQuality.max.rawValue
    }

    /// Channel 0 of `buf` (voice processing puts the processed signal there).
    func push(_ buf: AVAudioPCMBuffer, last: Bool = false) {
        guard let src = buf.floatChannelData, buf.frameLength > 0,
              let mono = AVAudioPCMBuffer(pcmFormat: monoFmt, frameCapacity: buf.frameLength) else { return }
        mono.frameLength = buf.frameLength
        memcpy(mono.floatChannelData![0], src[0], Int(buf.frameLength) * MemoryLayout<Float>.size)
        write(mono, last: last)
    }

    func write(_ mono: AVAudioPCMBuffer?, last: Bool) {
        let n = Double(mono?.frameLength ?? 0)
        let cap = AVAudioFrameCount(n * 16000 / monoFmt.sampleRate) + 256
        guard let out = AVAudioPCMBuffer(pcmFormat: outFmt, frameCapacity: cap) else { return }
        var fed = false
        var e: NSError?
        conv.convert(to: out, error: &e) { _, st in
            if fed || mono == nil {
                st.pointee = last ? .endOfStream : .noDataNow
                return nil
            }
            fed = true
            st.pointee = .haveData
            return mono
        }
        let k = Int(out.frameLength) * 2
        if k > 0, let p = out.int16ChannelData {
            FileHandle.standardOutput.write(Data(bytes: p[0], count: k))
            bytes += k
        }
    }
}

func micDisabled() -> Bool {
    let env = ProcessInfo.processInfo.environment
    return [env["GODTERM_NO_MIC"], env["CLAUDEGO_NO_MIC"]].contains { ($0 ?? "").isEmpty == false }
}

/// The running capture: an AVAudioEngine tapping the default input, built
/// again after a route change (a Bluetooth headset switching profile).
final class MicSession {
    let wantVP: Bool
    var engine = AVAudioEngine()
    var pipe: Pipe16k?
    var vpError: String?
    var restarts = 0

    init(vp: Bool) { wantVP = vp }

    /// One way of starting: voice processing or not, with or without the
    /// output side (mainMixerNode) in the graph.
    func attempt(vp: Bool, mixer: Bool) throws -> AVAudioFormat {
        engine = AVAudioEngine()
        let input = engine.inputNode
        if vp {
            try input.setVoiceProcessingEnabled(true)
            // Do not turn other audio (GodTerm's own talk back) down.
            if #available(macOS 14.0, *) {
                input.voiceProcessingOtherAudioDuckingConfiguration =
                    AVAudioVoiceProcessingOtherAudioDuckingConfiguration(enableAdvancedDucking: false, duckingLevel: .min)
            }
        }
        if mixer { _ = engine.mainMixerNode }
        let fmt = input.outputFormat(forBus: 0)
        guard fmt.sampleRate > 0, fmt.channelCount > 0 else { throw err("no input device") }
        let p = try Pipe16k(rate: fmt.sampleRate)
        pipe = p
        input.installTap(onBus: 0, bufferSize: 2048, format: fmt) { buf, _ in p.push(buf) }
        engine.prepare()
        do {
            try engine.start()
        } catch {
            input.removeTap(onBus: 0)
            engine.stop()
            throw error
        }
        return fmt
    }

    /// Voice processing first; when it cannot start (avfaudio -10875 with
    /// a Bluetooth headset in call mode), without it: the helper keeps the
    /// capture, so the headset mute gesture still comes here.
    func start() throws -> AVAudioFormat {
        let ladder: [(Bool, Bool)] = wantVP ? [(true, true), (true, false), (false, true), (false, false)] : [(false, true), (false, false)]
        var last: Error = err("no way to start the mic")
        for (vp, mixer) in ladder {
            do {
                return try attempt(vp: vp, mixer: mixer)
            } catch {
                last = error
                if vp { vpError = "\(error.localizedDescription)" }
                status(["event": "retry", "vp": vp, "mixer": mixer, "error": "\(error.localizedDescription)"])
            }
        }
        throw last
    }

    func started(_ fmt: AVAudioFormat) {
        let input = engine.inputNode
        var d = micModes()
        d["event"] = "started"
        d["vp"] = input.isVoiceProcessingEnabled
        d["agc"] = input.isVoiceProcessingEnabled ? input.isVoiceProcessingAGCEnabled : false
        d["in_rate"] = fmt.sampleRate
        d["in_channels"] = fmt.channelCount
        if let e = vpError { d["vp_error"] = e }
        status(d)
    }

    /// The route changed under the engine (it stops itself): start again
    /// on the new format, or end so GodTerm reopens the mic.
    func restart(_ why: String) {
        restarts += 1
        status(["event": "route", "what": why, "restarts": restarts])
        engine.inputNode.removeTap(onBus: 0)
        engine.stop()
        guard restarts <= 20 else {
            status(["event": "error", "error": "the audio route keeps changing"])
            exit(4)
        }
        do {
            let fmt = try start()
            status(["event": "restarted", "vp": engine.inputNode.isVoiceProcessingEnabled, "in_rate": fmt.sampleRate])
            observe()
        } catch {
            status(["event": "error", "error": "after a route change: \(error.localizedDescription)"])
            exit(4)
        }
    }

    var observer: NSObjectProtocol?

    func observe() {
        if let o = observer { NotificationCenter.default.removeObserver(o) }
        observer = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange, object: engine, queue: .main
        ) { [weak self] _ in
            self?.restart("engine configuration changed")
        }
    }
}

/// The headset's mute gesture (macOS 14+): delivered to the process that
/// records, which is this one. GodTerm takes a mute as a tap: mic off.
func registerMuteHandler() {
    if #available(macOS 14.0, *) {
        do {
            try AVAudioApplication.shared.setInputMuteStateChangeHandler { muted in
                status(["event": "input_mute", "muted": muted, "source": "AVAudioApplication"])
                return true
            }
            status(["event": "path", "path": "AVAudioApplication.inputMuteStateChangeHandler", "ok": true])
        } catch {
            status(["event": "path", "path": "AVAudioApplication.inputMuteStateChangeHandler", "ok": false, "error": "\(error.localizedDescription)"])
        }
        NotificationCenter.default.addObserver(
            forName: AVAudioApplication.inputMuteStateChangeNotification, object: nil, queue: .main
        ) { n in
            let m = (n.userInfo?[AVAudioApplication.muteStateKey] as? NSNumber)?.boolValue
            status(["event": "note", "what": "AVAudioApplication.inputMuteStateChangeNotification", "muted": m as Any])
        }
    } else {
        status(["event": "path", "path": "AVAudioApplication.inputMuteStateChangeHandler", "ok": false, "error": "needs macOS 14"])
    }
}

/// Diagnostics (--remote): the remote commands claimed from inside the
/// recording process, to see whether any arrive while the headset is in
/// call mode. Only for godterm tap-test: GodTerm itself owns Now Playing.
func registerRemoteCommands() {
    let c = MPRemoteCommandCenter.shared()
    let cmds: [(MPRemoteCommand, String)] = [
        (c.togglePlayPauseCommand, "togglePlayPause"), (c.playCommand, "play"),
        (c.pauseCommand, "pause"), (c.stopCommand, "stop"),
        (c.nextTrackCommand, "next"), (c.previousTrackCommand, "prev"),
    ]
    for (cmd, name) in cmds {
        cmd.isEnabled = true
        cmd.addTarget { _ in
            status(["event": "remote", "cmd": name, "source": "helper"])
            return .success
        }
    }
    MPNowPlayingInfoCenter.default().nowPlayingInfo = [
        MPMediaItemPropertyTitle: "GodTerm tap-test (mic open)",
        MPNowPlayingInfoPropertyPlaybackRate: 1.0,
    ]
    MPNowPlayingInfoCenter.default().playbackState = .playing
    status(["event": "path", "path": "MPRemoteCommandCenter (helper)", "ok": true])
}

func runMic(vp: Bool, remote: Bool) -> Never {
    if micDisabled() {
        status(["event": "error", "error": "the microphone is disabled (GODTERM_NO_MIC)"])
        exit(77)
    }
    registerMuteHandler()
    let s = MicSession(vp: vp)
    do {
        let fmt = try s.start()
        s.started(fmt)
        s.observe()
    } catch {
        status(["event": "error", "error": "\(error.localizedDescription)"])
        exit(3)
    }
    if remote { registerRemoteCommands() }
    // Report mic mode changes (Voice Isolation picked in Control Center).
    var last = modeName(AVCaptureDevice.activeMicrophoneMode)
    let t = DispatchSource.makeTimerSource(queue: .main)
    t.schedule(deadline: .now() + 1, repeating: 1)
    t.setEventHandler {
        _ = s
        let m = modeName(AVCaptureDevice.activeMicrophoneMode)
        if m != last {
            last = m
            status(["event": "mode", "mode": m])
        }
    }
    t.resume()
    signal(SIGTERM) { _ in exit(0) }
    dispatchMain()
}

/// The capture conversion and framing, from a file (smoke tests).
func runMicFile(_ path: String, realtime: Bool) -> Never {
    do {
        let f = try AVAudioFile(forReading: URL(fileURLWithPath: path))
        let fmt = f.processingFormat
        let p = try Pipe16k(rate: fmt.sampleRate)
        status(["event": "started", "vp": false, "in_rate": fmt.sampleRate, "in_channels": fmt.channelCount, "file": path])
        let chunk: AVAudioFrameCount = 2048
        while f.framePosition < f.length, let buf = AVAudioPCMBuffer(pcmFormat: fmt, frameCapacity: chunk) {
            try f.read(into: buf, frameCount: min(chunk, AVAudioFrameCount(f.length - f.framePosition)))
            if buf.frameLength == 0 { break }
            p.push(buf)
            if realtime { usleep(useconds_t(Double(buf.frameLength) / fmt.sampleRate * 1_000_000)) }
        }
        p.write(nil, last: true)
        status(["event": "end", "bytes": p.bytes])
        exit(0)
    } catch {
        status(["event": "error", "error": "\(error.localizedDescription)"])
        exit(3)
    }
}

let micArgs = CommandLine.arguments
if micArgs.contains("--mic-check") {
    // Never waits on the user or a stuck audio system: the authorization
    // is only read (never requested), and the mic mode query gets 2 s.
    func auth() -> String {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: return "authorized"
        case .denied: return "denied"
        case .restricted: return "restricted"
        case .notDetermined: return "not_determined"
        @unknown default: return "unknown"
        }
    }
    var d: [String: Any] = ["mic": true, "disabled": micDisabled(), "authorization": auth()]
    let done = DispatchSemaphore(value: 0)
    var modes: [String: Any] = [:]
    DispatchQueue.global().async {
        modes = micModes()
        done.signal()
    }
    if done.wait(timeout: .now() + 2) == .timedOut {
        d["mic"] = false
        d["error"] = "the audio system did not answer in 2 s"
    } else {
        d.merge(modes) { a, _ in a }
    }
    if let j = try? JSONSerialization.data(withJSONObject: d, options: [.sortedKeys]) {
        print(String(data: j, encoding: .utf8)!)
    }
    fflush(stdout)
    exit(0)
}
if micArgs.contains("--mic-modes") {
    AVCaptureDevice.showSystemUserInterface(.microphoneModes)
    RunLoop.main.run(until: Date().addingTimeInterval(1.5))
    exit(0)
}
if micArgs.contains("--mic") {
    runMic(vp: !micArgs.contains("--no-vp"), remote: micArgs.contains("--remote"))
}
if let i = micArgs.firstIndex(of: "--mic-file"), i + 1 < micArgs.count {
    runMicFile(micArgs[i + 1], realtime: micArgs.contains("--realtime"))
}

// MARK: speech recognition
let args = CommandLine.arguments
func flag(_ n: String) -> String? {
    guard let i = args.firstIndex(of: n), i + 1 < args.count else { return nil }
    return args[i + 1]
}

let localeId = flag("--locale") ?? "en-US"
let sema = DispatchSemaphore(value: 0)
Task {
    guard SpeechTranscriber.isAvailable, let loc = await locale(localeId) else {
        emit(Out(text: "", alternatives: [], confidence: nil, ms: 0, error: "on-device speech recognition is not available for \(localeId)"))
        exit(2)
    }
    if args.contains("--check") {
        let installed = await SpeechTranscriber.installedLocales.contains { $0.identifier(.bcp47) == loc.identifier(.bcp47) }
        print("{\"available\": true, \"locale\": \"\(loc.identifier(.bcp47))\", \"installed\": \(installed)}")
        exit(0)
    }
    if let f = flag("--file") {
        let ctx = flag("--context").map { $0.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) } } ?? []
        emit(await transcribe(path: f, loc: loc, context: ctx))
        exit(0)
    }
    if args.contains("--serve") {
        while let line = readLine() {
            guard let d = line.data(using: .utf8), let r = try? JSONDecoder().decode(Req.self, from: d) else {
                emit(Out(text: "", alternatives: [], confidence: nil, ms: 0, error: "bad request"))
                continue
            }
            emit(await transcribe(path: r.wav, loc: loc, context: r.context ?? []))
        }
        exit(0)
    }
    FileHandle.standardError.write("usage: godterm-speech --file x.wav | --serve | --check | --mic [--no-vp] | --mic-file x.wav | --mic-check | --mic-modes\n".data(using: .utf8)!)
    exit(64)
}
sema.wait()
