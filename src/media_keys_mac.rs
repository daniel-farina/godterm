//! macOS side of the headphone tap: ported from GrokTerm's media_keys.rs
//! (the one that works with Sony WH-1000XM5 double taps, AirPods and wired
//! EarPods).
//!
//! How a headphone press gets here: a Bluetooth tap is AVRCP play/pause,
//! a wired remote and the keyboard media key are HID; macOS hands either to
//! the Now Playing app through MediaRemote (MPRemoteCommandCenter). So:
//!
//! 1. Claim Now Playing: handlers on play, pause, toggle, stop, next and
//!    previous, and Now Playing info with a ticking elapsed time, always
//!    "playing" (a paused claim loses the slot to Music). No audio is
//!    played: the claim is metadata only, so Kokoro or Grok talk back and
//!    the mic are untouched.
//! 2. MediaRemote can go deaf after a mic or talk back cycle: republish
//!    every 1 s while listening (2 s otherwise), rebind the handlers every
//!    8 s while listening (20 s otherwise, and when talk back ends), and
//!    when the mic goes off, a hard reclaim (paused, then playing again,
//!    with fresh handlers). Presses for 1.5 s after it are ghosts (Gate).
//! 3. Backups that see the press without MediaRemote: an IOHID consumer
//!    page listener (needs Input Monitoring) and a CGEventTap on the
//!    play key (Accessibility or Input Monitoring). They only start when
//!    the terminal app already has that permission, so turning the setting
//!    on never brings up a permission prompt; they look again every 12 s.
//!
//! Main thread only: MediaPlayer is used from App's thread (the UI loop
//! runs on the process's main thread), and the remote command handlers run
//! on the main dispatch queue, so pump() runs the main run loop for an
//! instant each time round the UI loop. A press waits at most one UI loop
//! (capped at 100 ms while the setting is on).
//!
//! Unlike GrokTerm, no NSApplication is made and nothing is activated: a
//! terminal program must not steal focus from its terminal, and the claim
//! works without it (checked on macOS 27).
//!
//! Bluetooth note (GrokTerm's findings): when the headset's own mic is the
//! input, macOS switches it to the call profile (HFP) and most headsets
//! then stop sending play/pause until capture ends. The Mac's built in mic
//! keeps taps working.

use std::os::raw::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::MainThreadMarker;
use objc2_foundation::{NSDictionary, NSMutableDictionary, NSNumber, NSString};
use objc2_media_player::{
    MPMediaItemPropertyArtist, MPMediaItemPropertyPlaybackDuration, MPMediaItemPropertyTitle,
    MPNowPlayingInfoCenter, MPNowPlayingInfoPropertyElapsedPlaybackTime,
    MPNowPlayingInfoPropertyPlaybackRate, MPNowPlayingPlaybackState, MPRemoteCommandCenter,
    MPRemoteCommandEvent, MPRemoteCommandHandlerStatus,
};

use crate::app::AppEvent;
use crate::media_keys::{log, now_ms, Cmd, Press};

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after: u8) -> i32;
}

pub struct MacBackend {
    tx: Sender<AppEvent>,
    /// Handler tokens from addTargetWithHandler.
    tokens: Vec<Retained<AnyObject>>,
    listening: bool,
    speaking: bool,
    /// Elapsed time in Now Playing counts from here.
    session_ms: u64,
    published_ms: u64,
    rebound_ms: u64,
    reclaimed_ms: u64,
    stop: Arc<AtomicBool>,
    _threads: Vec<JoinHandle<()>>,
    released: bool,
    _main: MainThreadMarker,
}

/// Claim Now Playing (main thread only; None elsewhere).
pub fn start(tx: Sender<AppEvent>) -> Option<MacBackend> {
    let Some(mtm) = MainThreadMarker::new() else {
        log("not on the main thread: Now Playing is not claimed");
        return None;
    };
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = vec![];
    for (name, f) in [
        (
            "godterm-media-hid",
            hid_supervisor as fn(Sender<AppEvent>, Arc<AtomicBool>),
        ),
        ("godterm-media-tap", tap_supervisor),
    ] {
        let (tx, stop) = (tx.clone(), Arc::clone(&stop));
        if let Ok(h) = thread::Builder::new()
            .name(name.into())
            .spawn(move || f(tx, stop))
        {
            threads.push(h);
        }
    }
    let now = now_ms();
    let mut b = MacBackend {
        tx,
        tokens: vec![],
        listening: false,
        speaking: false,
        session_ms: now,
        published_ms: 0,
        rebound_ms: now,
        reclaimed_ms: 0,
        stop,
        _threads: threads,
        released: false,
        _main: mtm,
    };
    b.bind();
    b.publish();
    log("Now Playing claimed: headphone play/pause and the media key come to GodTerm");
    Some(b)
}

impl MacBackend {
    /// (Re)register the remote command handlers.
    fn bind(&mut self) {
        let center = unsafe { MPRemoteCommandCenter::sharedCommandCenter() };
        self.tokens.clear();
        let cmds = unsafe {
            [
                (
                    center.togglePlayPauseCommand(),
                    Cmd::TogglePlayPause,
                    "remote:togglePlayPause",
                ),
                (center.playCommand(), Cmd::Play, "remote:play"),
                (center.pauseCommand(), Cmd::Pause, "remote:pause"),
                (center.stopCommand(), Cmd::Stop, "remote:stop"),
                // Some headsets send a tap as next or previous track.
                (center.nextTrackCommand(), Cmd::Next, "remote:next"),
                (center.previousTrackCommand(), Cmd::Previous, "remote:prev"),
            ]
        };
        for (c, cmd, source) in cmds {
            let tx = self.tx.clone();
            // Only a message from here: publishing from inside the handler
            // breaks later deliveries (GrokTerm).
            let h = RcBlock::new(move |_e: NonNull<MPRemoteCommandEvent>| {
                let _ = tx.send(AppEvent::MediaKey(Press { cmd, source }));
                MPRemoteCommandHandlerStatus::Success
            });
            unsafe {
                c.removeTarget(None);
                c.setEnabled(true);
                self.tokens.push(c.addTargetWithHandler(&h));
            }
        }
        self.rebound_ms = now_ms();
    }

    fn publish(&mut self) {
        let title = if self.listening {
            "GodTerm voice: listening (tap to turn the mic off)"
        } else {
            "GodTerm voice: mic off (tap to listen)"
        };
        let elapsed = now_ms().saturating_sub(self.session_ms) as f64 / 1000.0;
        // A long duration so it looks like media going on.
        let duration = 3600.0;
        unsafe {
            let d = NSMutableDictionary::<NSString, AnyObject>::new();
            d.setObject_forKey(
                &*NSString::from_str(title),
                ProtocolObject::from_ref(MPMediaItemPropertyTitle),
            );
            d.setObject_forKey(
                &*NSString::from_str("GodTerm"),
                ProtocolObject::from_ref(MPMediaItemPropertyArtist),
            );
            d.setObject_forKey(
                &*NSNumber::new_f64(1.0),
                ProtocolObject::from_ref(MPNowPlayingInfoPropertyPlaybackRate),
            );
            d.setObject_forKey(
                &*NSNumber::new_f64(elapsed % (duration - 1.0)),
                ProtocolObject::from_ref(MPNowPlayingInfoPropertyElapsedPlaybackTime),
            );
            d.setObject_forKey(
                &*NSNumber::new_f64(duration),
                ProtocolObject::from_ref(MPMediaItemPropertyPlaybackDuration),
            );
            let center = MPNowPlayingInfoCenter::defaultCenter();
            let as_dict: &NSDictionary<NSString, AnyObject> = d.as_ref();
            center.setNowPlayingInfo(Some(as_dict));
            center.setPlaybackState(MPNowPlayingPlaybackState::Playing);
        }
        self.published_ms = now_ms();
    }

    /// After the mic goes off MediaRemote often stops delivering: bounce
    /// the playback state and bind fresh handlers.
    fn hard_reclaim(&mut self) {
        log("hard reclaim: paused, new handlers, playing");
        unsafe {
            MPNowPlayingInfoCenter::defaultCenter()
                .setPlaybackState(MPNowPlayingPlaybackState::Paused);
        }
        thread::sleep(Duration::from_millis(30));
        self.bind();
        self.session_ms = now_ms();
        self.reclaimed_ms = now_ms();
        self.publish();
    }
}

impl crate::media_keys::Backend for MacBackend {
    fn pump(&mut self, listening: bool, speaking: bool) {
        if self.released {
            return;
        }
        // The handlers run here (the main dispatch queue).
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.0, 0);
        }
        let now = now_ms();
        let was = std::mem::replace(&mut self.listening, listening);
        let was_speaking = std::mem::replace(&mut self.speaking, speaking);
        if was && !listening {
            self.hard_reclaim();
            return;
        }
        if listening && !was {
            self.session_ms = now;
            self.publish();
        }
        let every = if listening { 1_000 } else { 2_000 };
        if now.saturating_sub(self.published_ms) >= every {
            self.publish();
        }
        let rebind = if listening { 8_000 } else { 20_000 };
        let quiet = now.saturating_sub(self.reclaimed_ms) >= 5_000;
        if quiet && (now.saturating_sub(self.rebound_ms) >= rebind || (was_speaking && !speaking)) {
            self.bind();
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.stop.store(true, Ordering::Relaxed);
        unsafe {
            let center = MPRemoteCommandCenter::sharedCommandCenter();
            for c in [
                center.togglePlayPauseCommand(),
                center.playCommand(),
                center.pauseCommand(),
                center.stopCommand(),
                center.nextTrackCommand(),
                center.previousTrackCommand(),
            ] {
                c.removeTarget(None);
                c.setEnabled(false);
            }
            let info = MPNowPlayingInfoCenter::defaultCenter();
            info.setNowPlayingInfo(None);
            info.setPlaybackState(MPNowPlayingPlaybackState::Stopped);
        }
        self.tokens.clear();
        log("Now Playing cleared and the remote commands removed: Music and the others get the presses again");
    }

    fn reclaimed_ms(&self) -> u64 {
        self.reclaimed_ms
    }
}

impl Drop for MacBackend {
    fn drop(&mut self) {
        crate::media_keys::Backend::release(self);
    }
}

// ─── permissions (read only: never prompt) ─────────────────────────────────

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOHIDCheckAccess(request: u32) -> u32;
}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
}
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// Input Monitoring granted (kIOHIDRequestTypeListenEvent, granted = 0).
fn input_monitoring() -> bool {
    unsafe { IOHIDCheckAccess(1) == 0 }
}

fn tap_allowed() -> bool {
    unsafe { AXIsProcessTrusted() || CGPreflightListenEventAccess() }
}

/// Sleep up to 12 s, false when told to stop.
fn wait_retry(stop: &AtomicBool) -> bool {
    for _ in 0..48 {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(250));
    }
    !stop.load(Ordering::Relaxed)
}

fn send(tx: &Sender<AppEvent>, cmd: Cmd, source: &'static str) {
    let _ = tx.send(AppEvent::MediaKey(Press { cmd, source }));
}

// ─── IOHID consumer page ───────────────────────────────────────────────────

mod hid {
    use std::os::raw::c_void;

    pub type Ref = *mut c_void;
    pub const PAGE_CONSUMER: u32 = 0x0C;
    pub const USAGE_PLAY: u32 = 0xB0;
    pub const USAGE_PAUSE: u32 = 0xB1;
    pub const USAGE_PLAY_OR_PAUSE: u32 = 0xCD;

    pub type ValueCallback = Option<unsafe extern "C" fn(*mut c_void, i32, *mut c_void, Ref)>;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        pub fn IOHIDManagerCreate(allocator: *const c_void, options: u32) -> Ref;
        pub fn IOHIDManagerSetDeviceMatching(manager: Ref, matching: *const c_void);
        pub fn IOHIDManagerRegisterInputValueCallback(
            manager: Ref,
            cb: ValueCallback,
            ctx: *mut c_void,
        );
        pub fn IOHIDManagerScheduleWithRunLoop(manager: Ref, rl: Ref, mode: *const c_void);
        pub fn IOHIDManagerOpen(manager: Ref, options: u32) -> i32;
        pub fn IOHIDManagerClose(manager: Ref, options: u32) -> i32;
        pub fn IOHIDValueGetElement(value: Ref) -> Ref;
        pub fn IOHIDValueGetIntegerValue(value: Ref) -> i64;
        pub fn IOHIDElementGetUsagePage(element: Ref) -> u32;
        pub fn IOHIDElementGetUsage(element: Ref) -> u32;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(cf: *const c_void);
        pub fn CFRunLoopGetCurrent() -> Ref;
    }
}

fn hid_supervisor(tx: Sender<AppEvent>, stop: Arc<AtomicBool>) {
    let mut said = false;
    loop {
        if input_monitoring() {
            match run_hid(&tx, &stop) {
                Ok(()) => return,
                Err(e) => log(&format!("IOHIDManager: {e}")),
            }
        } else if !said {
            said = true;
            log("IOHID backup off: the terminal has no Input Monitoring permission (MediaRemote is the main path)");
        }
        if !wait_retry(&stop) {
            return;
        }
    }
}

fn run_hid(tx: &Sender<AppEvent>, stop: &AtomicBool) -> Result<(), String> {
    use hid::*;
    struct Ctx {
        tx: Sender<AppEvent>,
    }
    unsafe extern "C" fn cb(ctx: *mut c_void, _r: i32, _s: *mut c_void, value: Ref) {
        if ctx.is_null() || value.is_null() {
            return;
        }
        let ctx = &*(ctx as *const Ctx);
        let el = IOHIDValueGetElement(value);
        if el.is_null() || IOHIDValueGetIntegerValue(value) == 0 {
            return; // a release
        }
        if IOHIDElementGetUsagePage(el) != PAGE_CONSUMER {
            return;
        }
        let (cmd, source) = match IOHIDElementGetUsage(el) {
            USAGE_PLAY_OR_PAUSE => (Cmd::TogglePlayPause, "HID:PlayOrPause"),
            USAGE_PLAY => (Cmd::Play, "HID:Play"),
            USAGE_PAUSE => (Cmd::Pause, "HID:Pause"),
            _ => return,
        };
        send(&ctx.tx, cmd, source);
    }
    unsafe {
        let m = IOHIDManagerCreate(std::ptr::null(), 0);
        if m.is_null() {
            return Err("IOHIDManagerCreate failed".into());
        }
        // Every device: consumer only matching misses some Bluetooth paths.
        IOHIDManagerSetDeviceMatching(m, std::ptr::null());
        let ctx = Box::into_raw(Box::new(Ctx { tx: tx.clone() }));
        IOHIDManagerRegisterInputValueCallback(m, Some(cb), ctx as *mut c_void);
        IOHIDManagerScheduleWithRunLoop(m, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
        let open = IOHIDManagerOpen(m, 0);
        if open != 0 {
            CFRelease(m as *const c_void);
            drop(Box::from_raw(ctx));
            return Err(format!("IOHIDManagerOpen failed ({open})"));
        }
        log("IOHID backup on: consumer play/pause from every device");
        while !stop.load(Ordering::Relaxed) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
        }
        IOHIDManagerClose(m, 0);
        CFRelease(m as *const c_void);
        drop(Box::from_raw(ctx));
    }
    Ok(())
}

// ─── CGEventTap on the play key ────────────────────────────────────────────

mod tap {
    use std::os::raw::c_void;

    pub type Ref = *mut c_void;
    pub type Callback = Option<unsafe extern "C" fn(Ref, u32, Ref, *mut c_void) -> Ref>;
    pub const SESSION: u32 = 1;
    pub const HID: u32 = 0;
    pub const HEAD_INSERT: u32 = 0;
    pub const DEFAULT: u32 = 0;
    pub const LISTEN_ONLY: u32 = 1;
    pub const SYSTEM_DEFINED: u32 = 14;
    pub const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    pub const DISABLED_BY_USER: u32 = 0xFFFF_FFFD;
    pub const NX_KEYTYPE_PLAY: i64 = 16;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            mask: u64,
            cb: Callback,
            info: *mut c_void,
        ) -> Ref;
        pub fn CGEventTapEnable(tap: Ref, enable: bool);
        pub fn CGEventGetIntegerValueField(event: Ref, field: u32) -> i64;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFMachPortCreateRunLoopSource(alloc: *const c_void, port: Ref, order: isize) -> Ref;
        pub fn CFRunLoopGetCurrent() -> Ref;
        pub fn CFRunLoopAddSource(rl: Ref, source: Ref, mode: *const c_void);
        pub fn CFRelease(cf: *const c_void);
        pub static kCFRunLoopCommonModes: *const c_void;
    }

    /// The mask bit for a real event type (0..63). The tap disabled codes
    /// are near u32::MAX and must never be shifted in (GrokTerm crashed on
    /// that overflow).
    pub fn mask_bit(t: u32) -> u64 {
        if t >= 64 {
            0
        } else {
            1u64 << t
        }
    }
}

fn tap_supervisor(tx: Sender<AppEvent>, stop: Arc<AtomicBool>) {
    let mut said = false;
    loop {
        if tap_allowed() {
            match run_tap(&tx, &stop) {
                Ok(()) => return,
                Err(e) => log(&format!("CGEventTap: {e}")),
            }
        } else if !said {
            said = true;
            log("media key tap backup off: the terminal has no Accessibility or Input Monitoring permission");
        }
        if !wait_retry(&stop) {
            return;
        }
    }
}

struct TapCtx {
    tx: Sender<AppEvent>,
    port: std::sync::atomic::AtomicPtr<c_void>,
}

fn run_tap(tx: &Sender<AppEvent>, stop: &AtomicBool) -> Result<(), String> {
    use tap::*;
    unsafe extern "C" fn cb(_proxy: Ref, etype: u32, event: Ref, info: *mut c_void) -> Ref {
        if info.is_null() {
            return event;
        }
        let ctx = &*(info as *const TapCtx);
        if etype == DISABLED_BY_TIMEOUT || etype == DISABLED_BY_USER {
            let port = ctx.port.load(Ordering::Relaxed);
            if !port.is_null() {
                CGEventTapEnable(port, true);
            }
            return event;
        }
        if etype != SYSTEM_DEFINED {
            return event;
        }
        // data1 (field 55): key code in the high half, 0x0A down, 0x0B up.
        let data1 = CGEventGetIntegerValueField(event, 55);
        let code = (data1 & 0xFFFF_0000) >> 16;
        let flags = (data1 & 0x0000_FF00) >> 8;
        if code == NX_KEYTYPE_PLAY {
            if flags == 0x0A {
                send(&ctx.tx, Cmd::TogglePlayPause, "CGEventTap:PLAY");
            }
            // Swallowed (down and up) where the tap may: Music must not
            // start too.
            return std::ptr::null_mut();
        }
        event
    }
    let ctx = Box::into_raw(Box::new(TapCtx {
        tx: tx.clone(),
        port: std::sync::atomic::AtomicPtr::new(std::ptr::null_mut()),
    }));
    unsafe {
        let mask = mask_bit(SYSTEM_DEFINED);
        let mut port = std::ptr::null_mut();
        let mut kind = "";
        for (loc, opt, k) in [
            (SESSION, DEFAULT, "session"),
            (HID, DEFAULT, "hid"),
            (SESSION, LISTEN_ONLY, "session, listen only"),
        ] {
            port = CGEventTapCreate(loc, HEAD_INSERT, opt, mask, Some(cb), ctx as *mut c_void);
            if !port.is_null() {
                kind = k;
                break;
            }
        }
        if port.is_null() {
            drop(Box::from_raw(ctx));
            return Err("CGEventTapCreate failed".into());
        }
        (*ctx).port.store(port, Ordering::Relaxed);
        let src = CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0);
        if src.is_null() {
            CFRelease(port as *const c_void);
            drop(Box::from_raw(ctx));
            return Err("CFMachPortCreateRunLoopSource failed".into());
        }
        CFRunLoopAddSource(CFRunLoopGetCurrent(), src, kCFRunLoopCommonModes);
        CGEventTapEnable(port, true);
        log(&format!("media key tap backup on ({kind})"));
        while !stop.load(Ordering::Relaxed) {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0);
        }
        CGEventTapEnable(port, false);
        CFRelease(src as *const c_void);
        CFRelease(port as *const c_void);
        drop(Box::from_raw(ctx));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn tap_mask_never_overflows() {
        assert_eq!(super::tap::mask_bit(14), 1 << 14);
        assert_eq!(super::tap::mask_bit(super::tap::DISABLED_BY_TIMEOUT), 0);
    }
}
