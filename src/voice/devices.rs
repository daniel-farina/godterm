//! Which microphone the voice engine really records from, and whether it
//! is a good one for speech recognition (from `system_profiler`).

use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioDev {
    pub name: String,
    pub inputs: u32,
    /// "bluetooth", "builtin", "usb", "virtual", "aggregate", "unknown"...
    pub transport: String,
    pub rate: u32,
    pub default_input: bool,
}

pub fn parse(json: &str) -> Vec<AudioDev> {
    let Ok(v) = serde_json::from_str::<Value>(json) else {
        return vec![];
    };
    let mut out = vec![];
    for g in v["SPAudioDataType"].as_array().into_iter().flatten() {
        for it in g["_items"].as_array().into_iter().flatten() {
            let inputs = it["coreaudio_device_input"].as_u64().unwrap_or(0) as u32;
            if inputs == 0 {
                continue;
            }
            out.push(AudioDev {
                name: it["_name"].as_str().unwrap_or("").to_string(),
                inputs,
                transport: it["coreaudio_device_transport"]
                    .as_str()
                    .unwrap_or("")
                    .trim_start_matches("coreaudio_device_type_")
                    .to_string(),
                rate: it["coreaudio_device_srate"].as_u64().unwrap_or(0) as u32,
                default_input: it["coreaudio_default_audio_input_device"] == "spaudio_yes",
            });
        }
    }
    out
}

pub fn list() -> Vec<AudioDev> {
    std::process::Command::new("system_profiler")
        .args(["SPAudioDataType", "-json"])
        .output()
        .ok()
        .map(|o| parse(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// The device `device` ("default", an index or a name) resolves to.
pub fn chosen<'a>(devs: &'a [AudioDev], device: &str) -> Option<&'a AudioDev> {
    let d = device.trim();
    if d.is_empty() || d == "default" {
        return devs.iter().find(|x| x.default_input);
    }
    devs.iter().find(|x| x.name.eq_ignore_ascii_case(d))
}

/// What is wrong with recording speech from this device, and what to do.
pub fn advice(devs: &[AudioDev], device: &str) -> Option<String> {
    let d = chosen(devs, device)?;
    let better = devs
        .iter()
        .find(|x| x.transport == "builtin")
        .or_else(|| devs.iter().find(|x| x.transport == "usb"))
        .map(|x| x.name.clone());
    let switch = better
        .filter(|b| *b != d.name)
        .map(|b| format!(" Pick \"{b}\" in Settings > Voice > Input: microphone."));
    let why = match d.transport.as_str() {
        "bluetooth" => format!("the mic is \"{}\", a Bluetooth headset at {} kHz (call mode): recognition is worse and it drops the headphones to call quality while open.", d.name, d.rate / 1000),
        "virtual" => format!("the mic is \"{}\", a virtual device (no microphone signal unless something is routed into it).", d.name),
        "aggregate" => format!("the mic is \"{}\", an aggregate device; recording from one real microphone is more reliable.", d.name),
        "unknown" if d.name.to_lowercase().contains("iphone") || d.name.ends_with("Microphone") && d.transport == "unknown" => {
            format!("the mic is \"{}\", likely an iPhone (Continuity) microphone: it can drop out and adds latency.", d.name)
        }
        _ if d.rate > 0 && d.rate < 16000 => format!("the mic \"{}\" records at {} Hz, below what whisper needs (16 kHz).", d.name, d.rate),
        _ => return None,
    };
    Some(format!("Voice: {why}{}", switch.unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const J: &str = r#"{"SPAudioDataType":[{"_items":[
      {"_name":"WH-1000XM5","coreaudio_default_audio_input_device":"spaudio_yes","coreaudio_device_input":1,"coreaudio_device_srate":16000,"coreaudio_device_transport":"coreaudio_device_type_bluetooth"},
      {"_name":"WH-1000XM5","coreaudio_device_output":1,"coreaudio_device_transport":"coreaudio_device_type_bluetooth"},
      {"_name":"BlackHole 2ch","coreaudio_device_input":2,"coreaudio_device_srate":48000,"coreaudio_device_transport":"coreaudio_device_type_virtual"},
      {"_name":"MacBook Pro Microphone","coreaudio_device_input":1,"coreaudio_device_srate":48000,"coreaudio_device_transport":"coreaudio_device_type_builtin"},
      {"_name":"America Microphone","coreaudio_device_input":1,"coreaudio_device_srate":48000,"coreaudio_device_transport":"coreaudio_device_type_unknown"}]}]}"#;

    #[test]
    fn finds_the_default_and_warns() {
        let d = parse(J);
        assert_eq!(d.len(), 4, "outputs left out");
        assert_eq!(chosen(&d, "default").unwrap().name, "WH-1000XM5");
        let a = advice(&d, "default").unwrap();
        assert!(
            a.contains("Bluetooth") && a.contains("MacBook Pro Microphone"),
            "{a}"
        );
        assert!(advice(&d, "MacBook Pro Microphone").is_none());
        assert!(advice(&d, "BlackHole 2ch").unwrap().contains("virtual"));
        assert!(advice(&d, "America Microphone")
            .unwrap()
            .contains("Continuity"));
    }
}
