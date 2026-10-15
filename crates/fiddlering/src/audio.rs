//! Plays Fiddlesticks' voice lines and sound effects (WAVs prep converted from the player's League banks) through
//! XAudio2. Called only from the game thread.

use crate::log;
use lolread::cache::Cache;
use lolread::sheet;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::Media::Audio::XAudio2::*;
use windows::Win32::Media::Audio::*;

struct Wav {
    fmt: WAVEFORMATEX,
    data: Arc<Vec<u8>>,
}

pub struct Audio {
    _x: IXAudio2,
    master: IXAudio2MasteringVoice,
    playing: Vec<(IXAudio2SourceVoice, Arc<Vec<u8>>, &'static str)>,
    wavs: HashMap<String, Option<Wav>>,
    dir: PathBuf,
    files: BTreeMap<String, Vec<String>>,
    last: HashMap<&'static str, Instant>,
    seed: u64,
    pub volume: f32,
}

fn parse_wav(b: &[u8]) -> Option<Wav> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return None;
    }
    let mut at = 12;
    let mut fmt = None;
    let mut data = None;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().ok()?) as usize;
        let body = b.get(at + 8..(at + 8 + len).min(b.len()))?;
        if id == b"fmt " && body.len() >= 16 {
            let u16a = |o: usize| u16::from_le_bytes([body[o], body[o + 1]]);
            let u32a = |o: usize| u32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]]);
            fmt = Some(WAVEFORMATEX {
                wFormatTag: u16a(0),
                nChannels: u16a(2),
                nSamplesPerSec: u32a(4),
                nAvgBytesPerSec: u32a(8),
                nBlockAlign: u16a(12),
                wBitsPerSample: u16a(14),
                cbSize: 0,
            });
        } else if id == b"data" {
            data = Some(body.to_vec());
        }
        at += 8 + len + (len & 1);
    }
    let fmt = fmt?;
    if fmt.wFormatTag != 1 {
        return None;
    }
    Some(Wav { fmt, data: Arc::new(data?) })
}

impl Audio {
    pub fn new(cache: &Cache, dir: PathBuf) -> windows::core::Result<Self> {
        unsafe {
            let mut x: Option<IXAudio2> = None;
            XAudio2CreateWithVersionInfo(&mut x, 0, XAUDIO2_DEFAULT_PROCESSOR, 0)?;
            let x = x.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            let mut master: Option<IXAudio2MasteringVoice> = None;
            x.CreateMasteringVoice(&mut master, 0, 0, 0, None, None, AudioCategory_GameEffects)?;
            let master = master.ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
            let seed = std::process::id() as u64 ^ (Instant::now().elapsed().as_nanos() as u64) ^ 0x9e3779b97f4a7c15;
            Ok(Audio {
                _x: x,
                master,
                playing: vec![],
                wavs: HashMap::new(),
                dir,
                files: cache.sounds.clone(),
                last: HashMap::new(),
                seed,
                volume: lolread::sheet::tuning::AUDIO_VOLUME,
            })
        }
    }

    fn rand(&mut self) -> f32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        (self.seed % 10000) as f32 / 10000.0
    }

    /// Plays a sounds-sheet row (respecting its chance and minimum interval). Returns whether it played.
    pub fn play(&mut self, id: &str) -> bool {
        self.play_secs(id).is_some()
    }

    /// Like `play`, returning how long the sound lasts in seconds.
    pub fn play_secs(&mut self, id: &str) -> Option<f32> {
        let Some(row) = sheet::SOUNDS.iter().find(|s| s.id == id) else { return None };
        if let Some(t) = self.last.get(row.id) {
            if t.elapsed().as_secs_f32() < row.min_interval {
                return None;
            }
        }
        if self.rand() > row.chance {
            return None;
        }
        let r = self.rand();
        let Some(list) = self.files.get(row.id).filter(|l| !l.is_empty()) else { return None };
        let file = list[(r * list.len() as f32) as usize % list.len()].clone();
        let dir = self.dir.clone();
        let wav = self.wavs.entry(file.clone()).or_insert_with(|| std::fs::read(dir.join(&file)).ok().and_then(|b| parse_wav(&b)));
        let Some(w) = wav else { return None };
        let (fmt, data) = (w.fmt, w.data.clone());
        let secs = data.len() as f32 / fmt.nAvgBytesPerSec.max(1) as f32;
        unsafe {
            let mut v: Option<IXAudio2SourceVoice> = None;
            if let Err(e) = self._x.CreateSourceVoice(&mut v, &fmt, 0, 2.0, None, None, None) {
                log!("audio: {id}: {e:?}");
                return None;
            }
            let Some(v) = v else { return None };
            let buf = XAUDIO2_BUFFER {
                Flags: XAUDIO2_END_OF_STREAM,
                AudioBytes: data.len() as u32,
                pAudioData: data.as_ptr(),
                ..Default::default()
            };
            let _ = v.SetVolume(self.volume * row.volume, 0);
            if v.SubmitSourceBuffer(&buf, None).is_err() || v.Start(0, 0).is_err() {
                v.DestroyVoice();
                return None;
            }
            self.playing.push((v, data, row.id));
        }
        self.last.insert(row.id, Instant::now());
        Some(secs)
    }

    /// Switches to another skin's sounds (ids it lacks keep playing the base skin's).
    pub fn set_sounds(&mut self, sounds: &std::collections::BTreeMap<String, Vec<String>>) {
        self.files = sounds.clone();
    }

    /// Cuts every playing sound whose sheet id starts with `prefix` (a channel that ended early).
    pub fn stop(&mut self, prefix: &str) {
        self.playing.retain(|(v, _, id)| unsafe {
            if id.starts_with(prefix) {
                let _ = v.Stop(0, 0);
                v.DestroyVoice();
                false
            } else {
                true
            }
        });
    }

    pub fn update(&mut self) {
        self.playing.retain(|(v, _, _)| unsafe {
            let mut st = XAUDIO2_VOICE_STATE::default();
            v.GetState(&mut st, XAUDIO2_VOICE_NOSAMPLESPLAYED);
            if st.BuffersQueued == 0 {
                v.DestroyVoice();
                false
            } else {
                true
            }
        });
        let _ = &self.master;
    }
}
