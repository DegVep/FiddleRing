//! FiddleRing: play Elden Ring as League of Legends' Fiddlesticks. Loaded into the game by ModEngine2.

mod anim;
mod audio;
mod font;
mod fx;
mod game;
mod gfx;
#[macro_use]
mod log;
mod scene;

use eldenring::cs::{CSTaskGroupIndex, CSTaskImp};
use eldenring::fd4::FD4TaskData;
use fromsoftware_shared::SharedTaskImpExt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use windows::Win32::Foundation::{HMODULE, HWND, LPARAM};
use windows::Win32::System::LibraryLoader::{GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, GetModuleFileNameW, GetModuleHandleExW};
use windows::Win32::System::Threading::{CreateThread, THREAD_CREATION_FLAGS};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetWindowThreadProcessId, IsWindowVisible};
use windows::core::BOOL;


/// Starts FiddleRing when the DLL loads (a CRT initializer: runs after the runtime is set up, under the loader lock,
/// so it only starts a thread).
#[used]
#[unsafe(link_section = ".CRT$XCU")]
static INIT: extern "C" fn() = on_load;

extern "C" fn on_load() {
    unsafe {
        let _ = CreateThread(None, 0, Some(boot_thread), None, THREAD_CREATION_FLAGS(0), None);
    }
}

unsafe extern "system" fn boot_thread(_: *mut std::ffi::c_void) -> u32 {
    if catch_unwind(boot).is_err() {
        log!("boot panicked");
    }
    0
}

fn dll_dir() -> PathBuf {
    let mut module = HMODULE::default();
    let mut buf = [0u16; 1024];
    let n = unsafe {
        let _ = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(dll_dir as *const () as _),
            &mut module,
        );
        GetModuleFileNameW(Some(module), &mut buf)
    } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..n])).parent().map(|p| p.to_path_buf()).unwrap_or_default()
}

fn load_cache() -> Option<lolread::cache::Cache> {
    let path = log::data_dir().join("cache").join("fiddlesticks.bin");
    if !path.exists() {
        // Melty's setup step normally made it; otherwise build it now from the player's League
        let prep = dll_dir().join("fiddlering-prep.exe");
        log!("cache missing, running {}", prep.display());
        match std::process::Command::new(&prep).output() {
            Ok(o) => log!("prep: {}{}", String::from_utf8_lossy(&o.stdout).trim(), String::from_utf8_lossy(&o.stderr).trim()),
            Err(e) => log!("prep failed to start: {e}"),
        }
    }
    match std::fs::read(&path).map_err(anyhow::Error::from).and_then(|b| lolread::cache::load(&b)) {
        Ok(c) => {
            log!("cache: League {} ({}), {} joints, {} clips", c.league_version, c.locale, c.model.joints.len(), c.clips.len());
            Some(c)
        }
        Err(e) => {
            log!("cache {} unreadable: {e:#}", path.display());
            None
        }
    }
}

unsafe extern "system" fn has_window(h: HWND, found: LPARAM) -> BOOL {
    let mut pid = 0u32;
    unsafe {
        GetWindowThreadProcessId(h, Some(&mut pid));
        if pid == std::process::id() && IsWindowVisible(h).as_bool() {
            *(found.0 as *mut bool) = true;
            return false.into();
        }
    }
    true.into()
}

struct SendGame(game::Game);
unsafe impl Send for SendGame {}
impl SendGame {
    fn tick(&mut self) {
        self.0.tick()
    }
}

/// Logs where a fatal exception happened (addresses inside fiddlering.dll as RVAs) before the game dies.
unsafe extern "system" fn on_exception(info: *mut windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS) -> i32 {
    unsafe {
        let code = (*(*info).ExceptionRecord).ExceptionCode.0 as u32;
        if code == 0xc00000fd || code == 0xc0000005 {
            let base = dll_base();
            let mut frames = [std::ptr::null_mut(); 48];
            let n = windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace(0, &mut frames, None) as usize;
            let at = (*(*info).ExceptionRecord).ExceptionAddress as usize;
            let mut m = HMODULE::default();
            let mut name = [0u16; 260];
            let _ = GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(at as _), &mut m);
            let nl = GetModuleFileNameW(Some(m), &mut name) as usize;
            let mut s = format!("EXCEPTION {code:#x} at {at:#x} in {} (+{:#x}), stack:", String::from_utf16_lossy(&name[..nl]), at - m.0 as usize);
            for f in &frames[..n.min(14)] {
                let a = *f as usize;
                let mut fm = HMODULE::default();
                let mut fname = [0u16; 260];
                let _ = GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT, windows::core::PCWSTR(a as _), &mut fm);
                let fl = GetModuleFileNameW(Some(fm), &mut fname) as usize;
                let full = String::from_utf16_lossy(&fname[..fl]);
                s += &format!(" {}+{:#x}", full.rsplit(['\\', '/']).next().unwrap_or("?"), a.wrapping_sub(fm.0 as usize));
            }
            let rsp = (*(*info).ContextRecord).Rsp as *const usize;
            s += " | rsp:";
            for i in 0..12 {
                let a = *rsp.add(i);
                if a >= base && a < base + 0x400000 {
                    s += &format!(" fr+{:#x}", a - base);
                }
            }
            log::write(&s);
        }
    }
    0
}

fn dll_base() -> usize {
    let mut module = HMODULE::default();
    unsafe {
        let _ = GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            windows::core::PCWSTR(dll_base as *const () as _),
            &mut module,
        );
    }
    module.0 as usize
}

fn boot() {
    log::init();
    unsafe {
        windows::Win32::System::Diagnostics::Debug::AddVectoredExceptionHandler(1, Some(on_exception));
    }
    log!("FiddleRing {} starting in pid {}", env!("CARGO_PKG_VERSION"), std::process::id());
    let Some(cache) = load_cache() else { return };
    let cache = Arc::new(cache);

    // don't touch the game's singletons before its window is up (their lookup is cached forever)
    loop {
        let mut found = false;
        unsafe {
            let _ = EnumWindows(Some(has_window), LPARAM(&mut found as *mut bool as isize));
        }
        if found {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let task = match CSTaskImp::wait_for_instance(Duration::from_secs(120)) {
        Ok(t) => t,
        Err(e) => {
            log!("CSTask not found: {e:?}");
            return;
        }
    };
    log!("game ready");

    gfx::set_cache(cache.clone());
    if let Err(e) = gfx::install() {
        log!("gfx hooks failed: {e:?}");
    }

    let h1 = task.run_recurring(
        |_: &FD4TaskData| {
            let _ = catch_unwind(game::Game::block_inputs);
        },
        CSTaskGroupIndex::FrameBegin,
    );
    let mut g = SendGame(game::Game::new(cache));
    let mut broken = false;
    let h2 = task.run_recurring(
        move |_: &FD4TaskData| {
            if broken {
                return;
            }
            if catch_unwind(AssertUnwindSafe(|| g.tick())).is_err() {
                log!("game tick panicked; FiddleRing stops here");
                broken = true;
            }
        },
        CSTaskGroupIndex::DrawBegin,
    );
    let h3 = task.run_recurring(
        |_: &FD4TaskData| {
            let _ = catch_unwind(game::Game::capture_ability_inputs);
        },
        CSTaskGroupIndex::ChrIns_PreBehaviorSafe,
    );
    std::mem::forget(h3);
    std::mem::forget(h1);
    std::mem::forget(h2);
    log!("tasks registered");
}
