//! D3D12 renderer injected into Elden Ring's frame: vtable hooks on the swap chain's Present/ResizeBuffers and on
//! the command queue's ExecuteCommandLists (to learn the game's direct queue). Draws the skinned League model,
//! world sprites and the HUD on top of the game's back buffer right before it is presented.

use crate::log;
use crate::scene::{SCENE, Scene, Sprite, VfxMat};
use lolread::cache::{Cache, Tex};
use lolread::math::{self, M4};
use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::*;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::Memory::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

static ORIG_PRESENT: AtomicUsize = AtomicUsize::new(0);
static ORIG_RESIZE: AtomicUsize = AtomicUsize::new(0);
static QUEUE_VTABLE: AtomicUsize = AtomicUsize::new(0);

struct Shared {
    cache: Option<Arc<Cache>>,
    r: Option<Renderer>,
    failed: bool,
}
static STATE: Mutex<Shared> = Mutex::new(Shared { cache: None, r: None, failed: false });

pub fn set_cache(c: Arc<Cache>) {
    STATE.lock().unwrap().cache = Some(c);
}

/// Inline detour on the function itself (vtable patches ping-pong with the Steam overlay's own hooks).
unsafe fn detour(obj: *mut c_void, index: usize, hook: usize, orig: &AtomicUsize) -> Result<()> {
    unsafe {
        let target = *(*(obj as *const *const usize)).add(index);
        let d = retour::RawDetour::new(target as *const (), hook as *const ()).map_err(|e| {
            log!("gfx: detour {index}: {e}");
            Error::from(E_FAIL)
        })?;
        orig.store(d.trampoline() as *const () as usize, Ordering::SeqCst);
        d.enable().map_err(|_| Error::from(E_FAIL))?;
        std::mem::forget(d);
    }
    Ok(())
}

/// Builds a throwaway device + swap chain to reach the vtables shared with the game's objects, then patches them.
pub fn install() -> Result<()> {
    unsafe {
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("STATIC"),
            w!("fiddlering"),
            WS_OVERLAPPED,
            0,
            0,
            64,
            64,
            None,
            None,
            None,
            None,
        )?;
        let mut dev: Option<ID3D12Device> = None;
        D3D12CreateDevice(None, D3D_FEATURE_LEVEL_11_0, &mut dev)?;
        let dev = dev.ok_or_else(|| Error::from(E_FAIL))?;
        let queue: ID3D12CommandQueue =
            dev.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC { Type: D3D12_COMMAND_LIST_TYPE_DIRECT, ..Default::default() })?;
        let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
        let desc = DXGI_SWAP_CHAIN_DESC1 {
            Width: 64,
            Height: 64,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let sc = factory.CreateSwapChainForHwnd(&queue, hwnd, &desc, None, None)?;
        let sc_raw = sc.as_raw();
        detour(sc_raw, 8, present_hook as *const () as usize, &ORIG_PRESENT)?;
        detour(sc_raw, 13, resize_hook as *const () as usize, &ORIG_RESIZE)?;
        QUEUE_VTABLE.store(*(queue.as_raw() as *const usize), Ordering::SeqCst);
        drop(sc);
        drop(queue);
        drop(dev);
        let _ = DestroyWindow(hwnd);
    }
    log!("gfx: hooks installed");
    Ok(())
}

/// The swap chain keeps the game's command queue among its fields: find the pointer whose vtable is a command
/// queue's (no ExecuteCommandLists hook, which loops with the Steam overlay's own).
fn find_queue(sc: *mut c_void) -> Option<ID3D12CommandQueue> {
    let vt = QUEUE_VTABLE.load(Ordering::Relaxed);
    let readable = |p: usize, len: usize| -> bool {
        let mut mbi = MEMORY_BASIC_INFORMATION::default();
        let n = unsafe { VirtualQuery(Some(p as *const c_void), &mut mbi, std::mem::size_of::<MEMORY_BASIC_INFORMATION>()) };
        n != 0 && mbi.State == MEM_COMMIT && (mbi.Protect.0 & 0x66) != 0 && p + len <= mbi.BaseAddress as usize + mbi.RegionSize
    };
    for i in 0..512usize {
        let slot = sc as usize + i * 8;
        if !readable(slot, 8) {
            break;
        }
        let cand = unsafe { *(slot as *const usize) };
        if cand < 0x10000 || cand % 8 != 0 || !readable(cand, 8) {
            continue;
        }
        if unsafe { *(cand as *const usize) } != vt {
            continue;
        }
        let raw = cand as *mut c_void;
        if let Some(q) = unsafe { ID3D12CommandQueue::from_raw_borrowed(&raw) } {
            if unsafe { q.GetDesc() }.Type == D3D12_COMMAND_LIST_TYPE_DIRECT {
                log!("gfx: game queue found at swap chain +{:#x}", i * 8);
                return Some(q.clone());
            }
        }
    }
    None
}

unsafe extern "system" fn present_hook(this: *mut c_void, sync: u32, flags: u32) -> HRESULT {
    before_present(this, flags);
    let f: extern "system" fn(*mut c_void, u32, u32) -> HRESULT = unsafe { std::mem::transmute(ORIG_PRESENT.load(Ordering::Relaxed)) };
    f(this, sync, flags)
}

unsafe extern "system" fn resize_hook(this: *mut c_void, n: u32, w: u32, h: u32, fmt: DXGI_FORMAT, fl: u32) -> HRESULT {
    release_renderer();
    let f: extern "system" fn(*mut c_void, u32, u32, u32, DXGI_FORMAT, u32) -> HRESULT =
        unsafe { std::mem::transmute(ORIG_RESIZE.load(Ordering::Relaxed)) };
    f(this, n, w, h, fmt, fl)
}

/// Drops the GPU renderer; the next frame rebuilds it from the current cache (a new skin).
pub fn reset() {
    release_renderer();
}

fn release_renderer() {
    let r = std::panic::catch_unwind(|| {
        let mut st = STATE.lock().unwrap();
        if let Some(r) = st.r.take() {
            r.wait_idle();
            log!("gfx: swap chain resized, renderer released");
        }
    });
    if r.is_err() {
        log!("gfx: panic while releasing");
    }
}

/// The game presents from a thread with little stack left, so the frame is drawn on our own thread while the
/// presenting thread waits.
struct Job(usize);
static WORKER: Mutex<Option<(std::sync::mpsc::Sender<Job>, std::sync::mpsc::Receiver<()>)>> = Mutex::new(None);

fn before_present(this: *mut c_void, flags: u32) {
    if flags & DXGI_PRESENT_TEST.0 != 0 {
        return;
    }
    let Ok(mut w) = WORKER.lock() else { return };
    if w.is_none() {
        let (job_tx, job_rx) = std::sync::mpsc::channel::<Job>();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let spawned = std::thread::Builder::new().name("fiddlering-draw".into()).stack_size(8 << 20).spawn(move || {
            while let Ok(Job(sc)) = job_rx.recv() {
                draw_frame(sc as *mut c_void);
                if done_tx.send(()).is_err() {
                    break;
                }
            }
        });
        if spawned.is_err() {
            return;
        }
        *w = Some((job_tx, done_rx));
    }
    if let Some((tx, rx)) = w.as_ref() {
        if tx.send(Job(this as usize)).is_ok() {
            let _ = rx.recv();
        }
    }
}

fn draw_frame(this: *mut c_void) {
    let r = std::panic::catch_unwind(|| {
        let mut st = STATE.lock().unwrap();
        if st.failed {
            return;
        }
        let Some(sc) = (unsafe { IDXGISwapChain3::from_raw_borrowed(&this) }) else { return };
        if st.r.as_ref().map(|r| r.swapchain != this as usize).unwrap_or(false) {
            if let Some(r) = st.r.take() {
                r.wait_idle();
            }
        }
        if st.r.is_none() {
            let Some(cache) = st.cache.clone() else { return };
            let Some(queue) = find_queue(this) else {
                log!("gfx: game command queue not found, drawing disabled");
                st.failed = true;
                return;
            };
            match Renderer::new(sc, queue, &cache, this as usize) {
                Ok(r) => {
                    log!("gfx: renderer ready {}x{} format {:?} buffers {}", r.width, r.height, r.format, r.slots.len());
                    st.r = Some(r);
                }
                Err(e) => {
                    log!("gfx: renderer init failed: {e:?}");
                    st.failed = true;
                    return;
                }
            }
        }
        let scene = SCENE.lock().unwrap().clone();
        if let (Some(r), Some(scene)) = (st.r.as_mut(), scene) {
            if let Err(e) = r.draw(sc, &scene) {
                log!("gfx: draw failed: {e:?}");
                st.failed = true;
            }
        }
    });
    if r.is_err() {
        log!("gfx: panic in Present hook, drawing disabled");
        if let Ok(mut st) = STATE.lock() {
            st.failed = true;
        }
    }
}

const SHADERS: &str = r#"
#pragma pack_matrix(row_major)
cbuffer C : register(b0) { float4x4 mvp; float4x4 world; float4 tint; float4 light; };
cbuffer P : register(b1) { float4x4 pal[128]; };
Texture2D tex : register(t0);
Texture2D tex2 : register(t1);
Texture2D ero : register(t2);
SamplerState smp : register(s0);
SamplerState smpc : register(s1);
struct VI { float3 p : POSITION; float3 n : NORMAL; float2 uv : TEXCOORD; uint4 j : BLENDINDICES; float4 w : BLENDWEIGHT; };
struct VO { float4 p : SV_Position; float3 n : NORMAL; float2 uv : TEXCOORD; };
VO vs(VI i) {
    float4 p = 0; float3 n = 0;
    [unroll] for (int k = 0; k < 4; k++) {
        p += mul(float4(i.p, 1), pal[i.j[k]]) * i.w[k];
        n += mul(i.n, (float3x3)pal[i.j[k]]) * i.w[k];
    }
    VO o; o.p = mul(float4(p.xyz, 1), mvp); o.n = mul(n, (float3x3)world); o.uv = i.uv; return o;
}
float4 ps(VO i) : SV_Target {
    float4 c = tex.Sample(smp, i.uv);
    clip(c.a - 0.3);
    float l = light.w + (1 - light.w) * saturate(dot(normalize(i.n), normalize(light.xyz)));
    return float4(c.rgb * l * tint.rgb, 1);
}
struct SI { float4 p : POSITION; float2 uv : TEXCOORD0; float4 c : COLOR; float2 uv2 : TEXCOORD1; float2 e : TEXCOORD2; };
struct SO { float4 p : SV_Position; float2 uv : TEXCOORD0; float4 c : COLOR; float2 uv2 : TEXCOORD1; float2 e : TEXCOORD2; };
SO svs(SI i) { SO o; o.p = i.p; o.uv = i.uv; o.c = i.c; o.uv2 = i.uv2; o.e = i.e; return o; }
float4 sps(SO i) : SV_Target {
    // tint.x / tint.y: the texture / multiply texture repeat (else clamped at their edges)
    float4 c = (tint.x > 0.5 ? tex.Sample(smp, i.uv) : tex.Sample(smpc, i.uv)) * i.c * (tint.y > 0.5 ? tex2.Sample(smp, i.uv2) : tex2.Sample(smpc, i.uv2));
    // League's alpha erosion: the map dissolves the particle as the threshold rises
    if (i.e.y > 0) c.a *= saturate((ero.Sample(smp, i.uv).r - i.e.x) / i.e.y);
    return c;
}
"#;

#[repr(C)]
#[derive(Clone, Copy)]
struct GpuVertex {
    pos: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    joints: [u16; 4],
    weights: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SpriteVertex {
    pos: [f32; 4],
    uv: [f32; 2],
    color: [f32; 4],
    uv2: [f32; 2],
    erode: [f32; 2],
}

/// billboards, HUD and League particle triangles of one frame
const MAX_SPRITE_VERTS: usize = 6 * 32768;
/// Skinning palettes for every model drawn in a frame (Fiddlesticks + up to ~48 crows), 256-byte aligned.
const PALETTE_BYTES: usize = 512 * 1024;

struct Slot {
    alloc: ID3D12CommandAllocator,
    fence_value: u64,
    /// persistently mapped upload memory: palette (256-aligned) then sprite vertices
    upload: ID3D12Resource,
    mapped: *mut u8,
}

struct Renderer {
    swapchain: usize,
    width: u32,
    height: u32,
    format: DXGI_FORMAT,
    device: ID3D12Device,
    queue: ID3D12CommandQueue,
    list: ID3D12GraphicsCommandList,
    fence: ID3D12Fence,
    fence_next: u64,
    event: HANDLE,
    slots: Vec<Slot>,
    buffers: Vec<ID3D12Resource>,
    rtv_heap: ID3D12DescriptorHeap,
    rtv_size: u32,
    dsv_heap: ID3D12DescriptorHeap,
    _depth: ID3D12Resource,
    srv_heap: ID3D12DescriptorHeap,
    srv_size: u32,
    root: ID3D12RootSignature,
    pso_mesh: ID3D12PipelineState,
    /// 0 alpha, 1 additive, 2 premultiplied, 3 screen, 4 multiply
    pso_blend: Vec<ID3D12PipelineState>,
    meshes: Vec<Mesh>,
    _textures: Vec<ID3D12Resource>,
    tex_index: HashMap<String, u32>,
}

/// One skinned League model on the GPU: buffers and (first index, count, texture key) per submesh.
struct Mesh {
    vb: ID3D12Resource,
    ib: ID3D12Resource,
    index_count: u32,
    submeshes: Vec<(u32, u32, String)>,
}

// Only touched under STATE's lock.
unsafe impl Send for Renderer {}

fn heap(t: D3D12_HEAP_TYPE) -> D3D12_HEAP_PROPERTIES {
    D3D12_HEAP_PROPERTIES { Type: t, ..Default::default() }
}

fn buffer_desc(size: u64) -> D3D12_RESOURCE_DESC {
    D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
        Width: size,
        Height: 1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
        ..Default::default()
    }
}

unsafe fn upload_buffer(dev: &ID3D12Device, size: u64) -> Result<ID3D12Resource> {
    let mut r: Option<ID3D12Resource> = None;
    unsafe {
        dev.CreateCommittedResource(
            &heap(D3D12_HEAP_TYPE_UPLOAD),
            D3D12_HEAP_FLAG_NONE,
            &buffer_desc(size),
            D3D12_RESOURCE_STATE_GENERIC_READ,
            None,
            &mut r,
        )?;
    }
    r.ok_or_else(|| Error::from(E_FAIL))
}

unsafe fn fill(res: &ID3D12Resource, bytes: &[u8]) -> Result<()> {
    unsafe {
        let mut p = std::ptr::null_mut();
        res.Map(0, None, Some(&mut p))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        res.Unmap(0, None);
    }
    Ok(())
}

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

fn compile(entry: &str, target: &str) -> Result<ID3DBlob> {
    let mut code = None;
    let mut err = None;
    let e = std::ffi::CString::new(entry).unwrap();
    let t = std::ffi::CString::new(target).unwrap();
    let r = unsafe {
        D3DCompile(
            SHADERS.as_ptr() as _,
            SHADERS.len(),
            s!("fiddlering"),
            None,
            None,
            PCSTR(e.as_ptr() as _),
            PCSTR(t.as_ptr() as _),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut err),
        )
    };
    if let Err(x) = r {
        if let Some(b) = err {
            let msg = unsafe { std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()) };
            log!("gfx: shader {entry}: {}", String::from_utf8_lossy(msg));
        }
        return Err(x);
    }
    code.ok_or_else(|| Error::from(E_FAIL))
}

fn blob_bytes(b: &ID3DBlob) -> D3D12_SHADER_BYTECODE {
    unsafe { D3D12_SHADER_BYTECODE { pShaderBytecode: b.GetBufferPointer(), BytecodeLength: b.GetBufferSize() } }
}

fn transition(res: &ID3D12Resource, before: D3D12_RESOURCE_STATES, after: D3D12_RESOURCE_STATES) -> D3D12_RESOURCE_BARRIER {
    D3D12_RESOURCE_BARRIER {
        Type: D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: unsafe { std::mem::transmute_copy(res) },
                Subresource: D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}

impl Renderer {
    fn new(sc: &IDXGISwapChain3, queue: ID3D12CommandQueue, cache: &Cache, sc_ptr: usize) -> Result<Self> {
        unsafe {
            let device: ID3D12Device = sc.GetDevice()?;
            let desc = sc.GetDesc1()?;
            let n = desc.BufferCount.max(1);
            let format = match desc.Format {
                DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM,
                DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM,
                f => f,
            };

            let rtv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                NumDescriptors: n,
                ..Default::default()
            })?;
            let rtv_size = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_RTV);
            let mut buffers = vec![];
            for i in 0..n {
                let b: ID3D12Resource = sc.GetBuffer(i)?;
                let mut h = rtv_heap.GetCPUDescriptorHandleForHeapStart();
                h.ptr += (i * rtv_size) as usize;
                let rd = D3D12_RENDER_TARGET_VIEW_DESC { Format: format, ViewDimension: D3D12_RTV_DIMENSION_TEXTURE2D, ..Default::default() };
                device.CreateRenderTargetView(&b, Some(&rd), h);
                buffers.push(b);
            }

            let dsv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_DSV,
                NumDescriptors: 1,
                ..Default::default()
            })?;
            let mut depth: Option<ID3D12Resource> = None;
            device.CreateCommittedResource(
                &heap(D3D12_HEAP_TYPE_DEFAULT),
                D3D12_HEAP_FLAG_NONE,
                &D3D12_RESOURCE_DESC {
                    Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: desc.Width as u64,
                    Height: desc.Height,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: DXGI_FORMAT_D32_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Flags: D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
                    ..Default::default()
                },
                D3D12_RESOURCE_STATE_DEPTH_WRITE,
                Some(&D3D12_CLEAR_VALUE {
                    Format: DXGI_FORMAT_D32_FLOAT,
                    Anonymous: D3D12_CLEAR_VALUE_0 { DepthStencil: D3D12_DEPTH_STENCIL_VALUE { Depth: 1.0, Stencil: 0 } },
                }),
                &mut depth,
            )?;
            let depth = depth.ok_or_else(|| Error::from(E_FAIL))?;
            device.CreateDepthStencilView(&depth, None, dsv_heap.GetCPUDescriptorHandleForHeapStart());

            // root signature: b0 constants (40 dwords), b1 palette CBV, t0 table, s0 static sampler
            let srv_range = |reg: u32| D3D12_DESCRIPTOR_RANGE {
                RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                NumDescriptors: 1,
                BaseShaderRegister: reg,
                RegisterSpace: 0,
                OffsetInDescriptorsFromTableStart: 0,
            };
            let (range, range1, range2) = (srv_range(0), srv_range(1), srv_range(2));
            let params = [
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Constants: D3D12_ROOT_CONSTANTS { ShaderRegister: 0, RegisterSpace: 0, Num32BitValues: 40 },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        Descriptor: D3D12_ROOT_DESCRIPTOR { ShaderRegister: 1, RegisterSpace: 0 },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: &range },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: &range1 },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
                },
                D3D12_ROOT_PARAMETER {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    Anonymous: D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE { NumDescriptorRanges: 1, pDescriptorRanges: &range2 },
                    },
                    ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
                },
            ];
            let sampler = D3D12_STATIC_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_WRAP,
                MaxLOD: f32::MAX,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
                ..Default::default()
            };
            let clamp = D3D12_STATIC_SAMPLER_DESC {
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                ShaderRegister: 1,
                ..sampler
            };
            let samplers = [sampler, clamp];
            let rs_desc = D3D12_ROOT_SIGNATURE_DESC {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
            };
            let mut blob = None;
            D3D12SerializeRootSignature(&rs_desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, None)?;
            let blob = blob.ok_or_else(|| Error::from(E_FAIL))?;
            let root: ID3D12RootSignature =
                device.CreateRootSignature(0, std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()))?;

            let (vs, ps, svs, sps) = (compile("vs", "vs_5_0")?, compile("ps", "ps_5_0")?, compile("svs", "vs_5_0")?, compile("sps", "ps_5_0")?);
            let mesh_layout = [
                ("POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0),
                ("NORMAL", 0, DXGI_FORMAT_R32G32B32_FLOAT, 12),
                ("TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 24),
                ("BLENDINDICES", 0, DXGI_FORMAT_R16G16B16A16_UINT, 32),
                ("BLENDWEIGHT", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 40),
            ];
            let sprite_layout = [
                ("POSITION", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0),
                ("TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 16),
                ("COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 24),
                ("TEXCOORD", 1, DXGI_FORMAT_R32G32_FLOAT, 40),
                ("TEXCOORD", 2, DXGI_FORMAT_R32G32_FLOAT, 48),
            ];
            let pso = |layout: &[(&str, u32, DXGI_FORMAT, u32)], v: &ID3DBlob, p: &ID3DBlob, depth: bool, blend: Option<(D3D12_BLEND, D3D12_BLEND)>| -> Result<ID3D12PipelineState> {
                let names: Vec<std::ffi::CString> = layout.iter().map(|l| std::ffi::CString::new(l.0).unwrap()).collect();
                let elems: Vec<D3D12_INPUT_ELEMENT_DESC> = layout
                    .iter()
                    .zip(&names)
                    .map(|(l, n)| D3D12_INPUT_ELEMENT_DESC {
                        SemanticName: PCSTR(n.as_ptr() as _),
                        SemanticIndex: l.1,
                        Format: l.2,
                        InputSlot: 0,
                        AlignedByteOffset: l.3,
                        InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                        InstanceDataStepRate: 0,
                    })
                    .collect();
                let mut bs = D3D12_BLEND_DESC::default();
                bs.RenderTarget[0].RenderTargetWriteMask = D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8;
                if let Some((src, dst)) = blend {
                    let rt = &mut bs.RenderTarget[0];
                    rt.BlendEnable = true.into();
                    rt.SrcBlend = src;
                    rt.DestBlend = dst;
                    rt.BlendOp = D3D12_BLEND_OP_ADD;
                    rt.SrcBlendAlpha = D3D12_BLEND_ZERO;
                    rt.DestBlendAlpha = D3D12_BLEND_ONE;
                    rt.BlendOpAlpha = D3D12_BLEND_OP_ADD;
                    rt.LogicOp = D3D12_LOGIC_OP_NOOP;
                }
                let mut d = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                    pRootSignature: std::mem::transmute_copy(&root),
                    VS: blob_bytes(v),
                    PS: blob_bytes(p),
                    BlendState: bs,
                    SampleMask: u32::MAX,
                    RasterizerState: D3D12_RASTERIZER_DESC {
                        FillMode: D3D12_FILL_MODE_SOLID,
                        CullMode: D3D12_CULL_MODE_NONE,
                        DepthClipEnable: true.into(),
                        ..Default::default()
                    },
                    DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                        DepthEnable: depth.into(),
                        DepthWriteMask: if depth { D3D12_DEPTH_WRITE_MASK_ALL } else { D3D12_DEPTH_WRITE_MASK_ZERO },
                        DepthFunc: D3D12_COMPARISON_FUNC_LESS_EQUAL,
                        ..Default::default()
                    },
                    InputLayout: D3D12_INPUT_LAYOUT_DESC { pInputElementDescs: elems.as_ptr(), NumElements: elems.len() as u32 },
                    PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                    NumRenderTargets: 1,
                    DSVFormat: DXGI_FORMAT_D32_FLOAT,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    ..Default::default()
                };
                d.RTVFormats[0] = format;
                device.CreateGraphicsPipelineState(&d)
            };
            let pso_mesh = pso(&mesh_layout, &vs, &ps, true, None)?;
            let pso_blend = [
                (D3D12_BLEND_SRC_ALPHA, D3D12_BLEND_INV_SRC_ALPHA),
                (D3D12_BLEND_SRC_ALPHA, D3D12_BLEND_ONE),
                (D3D12_BLEND_ONE, D3D12_BLEND_INV_SRC_ALPHA),
                (D3D12_BLEND_SRC_ALPHA, D3D12_BLEND_INV_SRC_COLOR),
                (D3D12_BLEND_DEST_COLOR, D3D12_BLEND_ZERO),
            ]
            .iter()
            .map(|&b| pso(&sprite_layout, &svs, &sps, false, Some(b)))
            .collect::<Result<Vec<_>>>()?;

            // mesh buffers (upload heap: small, static, read straight by the GPU)
            let mut models: Vec<&lolread::cache::Model> = vec![&cache.model];
            if let Some((raven, _)) = &cache.raven {
                models.push(raven);
            }
            let mut meshes = vec![];
            for (mi, m) in models.iter().enumerate() {
                let verts: Vec<GpuVertex> =
                    m.vertices.iter().map(|v| GpuVertex { pos: v.pos, normal: v.normal, uv: v.uv, joints: v.joints, weights: v.weights }).collect();
                let vb = upload_buffer(&device, std::mem::size_of_val(verts.as_slice()).max(4) as u64)?;
                fill(&vb, as_bytes(&verts))?;
                let ib = upload_buffer(&device, (m.indices.len() * 2).max(2) as u64)?;
                fill(&ib, as_bytes(&m.indices))?;
                let submeshes = m.submeshes.iter().map(|s| (s.first_index, s.index_count, format!("m{mi}_{}", s.texture))).collect();
                meshes.push(Mesh { vb, ib, index_count: m.indices.len() as u32, submeshes });
            }

            let alloc0: ID3D12CommandAllocator = device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
            let list: ID3D12GraphicsCommandList = device.CreateCommandList(0, D3D12_COMMAND_LIST_TYPE_DIRECT, &alloc0, None)?;
            let fence: ID3D12Fence = device.CreateFence(0, D3D12_FENCE_FLAG_NONE)?;
            let event = CreateEventW(None, false, false, None)?;

            // textures: model, every league asset texture in the cache, white, font
            let mut texs: Vec<(String, Tex)> = vec![];
            for (mi, m) in models.iter().enumerate() {
                for (i, t) in m.textures.iter().enumerate() {
                    texs.push((format!("m{mi}_{i}"), t.clone()));
                }
            }
            for (k, t) in &cache.icons {
                texs.push((k.clone(), t.clone()));
            }
            for (k, t) in &cache.vfx.textures {
                texs.push((k.clone(), t.clone()));
            }
            texs.push(("white".into(), Tex { format: lolread::texture::DXGI_BGRA8, width: 4, height: 4, mips: vec![vec![255; 64]] }));
            texs.push(("font".into(), crate::font::atlas()));
            let srv_heap: ID3D12DescriptorHeap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: texs.len() as u32,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                NodeMask: 0,
            })?;
            let srv_size = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV);
            let mut textures = vec![];
            let mut uploads = vec![];
            let mut tex_index = HashMap::new();
            for (i, (name, t)) in texs.iter().enumerate() {
                let (res, up) = match create_texture(&device, &list, t) {
                    Ok(x) => x,
                    Err(e) => {
                        log!("gfx: texture {name} ({}x{} fmt {}) skipped: {e:?}", t.width, t.height, t.format);
                        continue;
                    }
                };
                let mut h = srv_heap.GetCPUDescriptorHandleForHeapStart();
                h.ptr += (i as u32 * srv_size) as usize;
                device.CreateShaderResourceView(&res, None, h);
                tex_index.insert(name.clone(), i as u32);
                textures.push(res);
                uploads.push(up);
            }
            list.Close()?;
            queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>()?)]);
            queue.Signal(&fence, 1)?;
            fence.SetEventOnCompletion(1, event)?;
            WaitForSingleObject(event, 5000);
            drop(uploads);

            let mut slots = vec![];
            for i in 0..n {
                let alloc = if i == 0 { alloc0.clone() } else { device.CreateCommandAllocator(D3D12_COMMAND_LIST_TYPE_DIRECT)? };
                let upload = upload_buffer(&device, (PALETTE_BYTES + MAX_SPRITE_VERTS * std::mem::size_of::<SpriteVertex>()) as u64)?;
                let mut p = std::ptr::null_mut();
                upload.Map(0, None, Some(&mut p))?;
                slots.push(Slot { alloc, fence_value: 0, upload, mapped: p as *mut u8 });
            }

            Ok(Renderer {
                swapchain: sc_ptr,
                width: desc.Width,
                height: desc.Height,
                format,
                device,
                queue,
                list,
                fence,
                fence_next: 2,
                event,
                slots,
                buffers,
                rtv_heap,
                rtv_size,
                dsv_heap,
                _depth: depth,
                srv_heap,
                srv_size,
                root,
                pso_mesh,
                pso_blend,
                meshes,
                _textures: textures,
                tex_index,
            })
        }
    }

    fn wait_idle(&self) {
        unsafe {
            let v = self.fence_next;
            if self.queue.Signal(&self.fence, v).is_ok() && self.fence.GetCompletedValue() < v {
                let _ = self.fence.SetEventOnCompletion(v, self.event);
                WaitForSingleObject(self.event, 2000);
            }
            let _ = CloseHandle(self.event);
        }
    }

    fn srv(&self, key: &str) -> Option<D3D12_GPU_DESCRIPTOR_HANDLE> {
        let i = *self.tex_index.get(key)?;
        let mut h = unsafe { self.srv_heap.GetGPUDescriptorHandleForHeapStart() };
        h.ptr += (i * self.srv_size) as u64;
        Some(h)
    }

    fn draw(&mut self, sc: &IDXGISwapChain3, scene: &Scene) -> Result<()> {
        unsafe {
            let idx = sc.GetCurrentBackBufferIndex() as usize;
            if idx >= self.slots.len() {
                return Ok(());
            }
            let slot = &self.slots[idx];
            if self.fence.GetCompletedValue() < slot.fence_value {
                self.fence.SetEventOnCompletion(slot.fence_value, self.event)?;
                WaitForSingleObject(self.event, 1000);
            }
            slot.alloc.Reset()?;
            self.list.Reset(&slot.alloc, None)?;
            let list = &self.list;
            let bb = &self.buffers[idx];
            list.ResourceBarrier(&[transition(bb, D3D12_RESOURCE_STATE_PRESENT, D3D12_RESOURCE_STATE_RENDER_TARGET)]);
            let mut rtv = self.rtv_heap.GetCPUDescriptorHandleForHeapStart();
            rtv.ptr += idx * self.rtv_size as usize;
            let dsv = self.dsv_heap.GetCPUDescriptorHandleForHeapStart();
            list.OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
            list.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 1.0, 0, None);
            let (w, h) = (self.width as f32, self.height as f32);
            list.RSSetViewports(&[D3D12_VIEWPORT { TopLeftX: 0., TopLeftY: 0., Width: w, Height: h, MinDepth: 0., MaxDepth: 1. }]);
            list.RSSetScissorRects(&[RECT { left: 0, top: 0, right: self.width as i32, bottom: self.height as i32 }]);
            list.SetGraphicsRootSignature(&self.root);
            list.SetDescriptorHeaps(&[Some(self.srv_heap.clone())]);
            list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);

            let view = math::inverse(&scene.cam);
            let aspect = w / h.max(1.0);
            crate::scene::ASPECT.store(aspect.to_bits(), std::sync::atomic::Ordering::Relaxed);
            let proj = perspective(scene.fov, aspect, scene.near.max(0.05), scene.far.max(100.0));
            let view_proj = math::mul(&view, &proj);
            let upload_gpu = slot.upload.GetGPUVirtualAddress();

            // light from above and slightly behind the camera; .w = ambient
            let cam_up = [scene.cam[4], scene.cam[5], scene.cam[6]];
            let back = [-scene.cam[8], -scene.cam[9], -scene.cam[10]];
            let light = [cam_up[0] + back[0] * 0.6, cam_up[1] + back[1] * 0.6, cam_up[2] + back[2] * 0.6, 0.55];
            let mut pal_at = 0usize;
            let mut bound: Option<usize> = None;
            list.SetPipelineState(&self.pso_mesh);
            for m in &scene.models {
                let Some(mesh) = self.meshes.get(m.model) else { continue };
                let bytes = m.palette.len().min(128) * 64;
                // the shader declares 128 matrices: keep a full 8 KB window inside the buffer
                if pal_at + 128 * 64 > PALETTE_BYTES {
                    break;
                }
                for (i, p) in m.palette.iter().take(128).enumerate() {
                    std::ptr::copy_nonoverlapping(p.as_ptr() as *const u8, slot.mapped.add(pal_at + i * 64), 64);
                }
                let mvp = math::mul(&m.world, &view_proj);
                let mut consts = [0f32; 40];
                consts[..16].copy_from_slice(&mvp);
                consts[16..32].copy_from_slice(&m.world);
                consts[32..36].copy_from_slice(&[1.0, 1.0, 1.0, 1.0]);
                consts[36..40].copy_from_slice(&light);
                list.SetGraphicsRoot32BitConstants(0, 40, consts.as_ptr() as _, 0);
                list.SetGraphicsRootConstantBufferView(1, upload_gpu + pal_at as u64);
                pal_at += bytes.div_ceil(256) * 256;
                if bound != Some(m.model) {
                    bound = Some(m.model);
                    list.IASetVertexBuffers(
                        0,
                        Some(&[D3D12_VERTEX_BUFFER_VIEW {
                            BufferLocation: mesh.vb.GetGPUVirtualAddress(),
                            SizeInBytes: mesh.vb.GetDesc().Width as u32,
                            StrideInBytes: std::mem::size_of::<GpuVertex>() as u32,
                        }]),
                    );
                    list.IASetIndexBuffer(Some(&D3D12_INDEX_BUFFER_VIEW {
                        BufferLocation: mesh.ib.GetGPUVirtualAddress(),
                        SizeInBytes: mesh.index_count * 2,
                        Format: DXGI_FORMAT_R16_UINT,
                    }));
                }
                for (i, (first, count, tex)) in mesh.submeshes.iter().enumerate() {
                    if !m.visible.get(i).copied().unwrap_or(false) {
                        continue;
                    }
                    let Some(t) = self.srv(tex) else { continue };
                    list.SetGraphicsRootDescriptorTable(2, t);
                    list.DrawIndexedInstanced(*count, 1, *first, 0, 0);
                }
            }

            // sprites: world ones projected here, HUD from the 1920x1080 reference frame (scaled by height, centred)
            let mut verts: Vec<SpriteVertex> = Vec::with_capacity(scene.world.len() * 6 + scene.screen.len() * 6);
            let mut batches: Vec<(VfxMat, u32, u32)> = vec![];
            let project = |p: [f32; 3]| -> [f32; 4] {
                let v = [p[0], p[1], p[2], 1.0];
                let mut o = [0f32; 4];
                for c in 0..4 {
                    o[c] = (0..4).map(|r| v[r] * view_proj[r * 4 + c]).sum();
                }
                o
            };
            let mut add_batch = |mat: VfxMat, start: u32, n: u32| match batches.last_mut() {
                Some(b) if b.0 == mat => b.2 += n,
                _ => batches.push((mat, start, n)),
            };
            for (mat, tris) in &scene.vfx {
                let n = tris.len() / 3 * 3;
                if n == 0 || verts.len() + n > MAX_SPRITE_VERTS {
                    continue;
                }
                let start = verts.len() as u32;
                for v in &tris[..n] {
                    verts.push(SpriteVertex { pos: project(v.pos), uv: v.uv, color: v.color, uv2: v.uv2, erode: v.erode });
                }
                add_batch(*mat, start, n as u32);
            }
            let mut push = |s: &Sprite, clip: [[f32; 4]; 4], verts: &mut Vec<SpriteVertex>| {
                if verts.len() + 6 > MAX_SPRITE_VERTS {
                    return;
                }
                let start = verts.len() as u32;
                for k in [0, 1, 2, 0, 2, 3] {
                    verts.push(SpriteVertex { pos: clip[k], uv: s.uv[k], color: s.color, uv2: [0.0; 2], erode: [0.0; 2] });
                }
                add_batch(VfxMat { tex: s.tex, mult: "white", erode: "white", blend: s.additive as u8, wrap: 3 }, start, 6);
            };
            for s in &scene.world {
                let clip = s.pos.map(project);
                if clip.iter().all(|c| c[3] <= 0.05) {
                    continue;
                }
                push(s, clip, &mut verts);
            }
            let scale = h / 1080.0;
            let xoff = (w - 1920.0 * scale) * 0.5;
            for s in &scene.screen {
                let clip = s.pos.map(|p| [((xoff + p[0] * scale) / w) * 2.0 - 1.0, 1.0 - (p[1] * scale / h) * 2.0, 0.0, 1.0]);
                push(s, clip, &mut verts);
            }
            if !verts.is_empty() {
                std::ptr::copy_nonoverlapping(verts.as_ptr() as *const u8, slot.mapped.add(PALETTE_BYTES), std::mem::size_of_val(verts.as_slice()));
                list.IASetVertexBuffers(
                    0,
                    Some(&[D3D12_VERTEX_BUFFER_VIEW {
                        BufferLocation: upload_gpu + PALETTE_BYTES as u64,
                        SizeInBytes: std::mem::size_of_val(verts.as_slice()) as u32,
                        StrideInBytes: std::mem::size_of::<SpriteVertex>() as u32,
                    }]),
                );
                let white = self.srv("white");
                for (mat, start, count) in batches {
                    let (Some(t), Some(m), Some(e)) = (self.srv(mat.tex).or(white), self.srv(mat.mult).or(white), self.srv(mat.erode).or(white)) else { continue };
                    list.SetPipelineState(&self.pso_blend[(mat.blend as usize).min(4)]);
                    list.SetGraphicsRootDescriptorTable(2, t);
                    list.SetGraphicsRootDescriptorTable(3, m);
                    list.SetGraphicsRootDescriptorTable(4, e);
                    let flags = [(mat.wrap & 1) as f32, ((mat.wrap >> 1) & 1) as f32];
                    list.SetGraphicsRoot32BitConstants(0, 2, flags.as_ptr() as _, 32);
                    list.DrawInstanced(count, 1, start, 0);
                }
            }

            // a requested screenshot: copy the finished frame (game + Fiddlesticks + HUD) to a readback buffer
            let mut readback = None;
            if let Some(path) = &scene.shot {
                let desc = bb.GetDesc();
                let mut fp = D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default();
                let mut total = 0u64;
                self.device.GetCopyableFootprints(&desc, 0, 1, 0, Some(&mut fp), None, None, Some(&mut total));
                let mut rb: Option<ID3D12Resource> = None;
                self.device.CreateCommittedResource(&heap(D3D12_HEAP_TYPE_READBACK), D3D12_HEAP_FLAG_NONE, &buffer_desc(total), D3D12_RESOURCE_STATE_COPY_DEST, None, &mut rb)?;
                let rb = rb.ok_or_else(|| Error::from(E_FAIL))?;
                list.ResourceBarrier(&[transition(bb, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
                let dst = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&rb),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: fp },
                };
                let src = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(bb),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
                list.ResourceBarrier(&[transition(bb, D3D12_RESOURCE_STATE_COPY_SOURCE, D3D12_RESOURCE_STATE_PRESENT)]);
                readback = Some((rb, fp, path.clone()));
            } else {
                list.ResourceBarrier(&[transition(bb, D3D12_RESOURCE_STATE_RENDER_TARGET, D3D12_RESOURCE_STATE_PRESENT)]);
            }
            list.Close()?;
            self.queue.ExecuteCommandLists(&[Some(list.cast::<ID3D12CommandList>()?)]);
            let v = self.fence_next;
            self.fence_next += 1;
            self.queue.Signal(&self.fence, v)?;
            self.slots[idx].fence_value = v;
            if let Some((rb, fp, path)) = readback {
                self.fence.SetEventOnCompletion(v, self.event)?;
                WaitForSingleObject(self.event, 2000);
                let mut p = std::ptr::null_mut();
                rb.Map(0, None, Some(&mut p))?;
                let (w, h, pitch) = (fp.Footprint.Width as usize, fp.Footprint.Height as usize, fp.Footprint.RowPitch as usize);
                let bgra = matches!(self.format, DXGI_FORMAT_B8G8R8A8_UNORM);
                let mut rgb = Vec::with_capacity(w * h * 3);
                for y in 0..h {
                    let row = std::slice::from_raw_parts((p as *const u8).add(y * pitch), w * 4);
                    for px in row.chunks_exact(4) {
                        if bgra {
                            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
                        } else {
                            rgb.extend_from_slice(&px[..3]);
                        }
                    }
                }
                rb.Unmap(0, None);
                let saved = std::fs::File::create(&path).map_err(|_| Error::from(E_FAIL)).and_then(|f| {
                    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
                    enc.set_color(png::ColorType::Rgb);
                    enc.set_depth(png::BitDepth::Eight);
                    enc.write_header().and_then(|mut wr| wr.write_image_data(&rgb)).map_err(|_| Error::from(E_FAIL))
                });
                log!("SHOT {} {}", path, if saved.is_ok() { "saved" } else { "failed" });
            }
        }
        Ok(())
    }
}

/// Left-handed perspective, depth 0..1 (row-vector convention).
pub(crate) fn perspective(fov_y: f32, aspect: f32, near: f32, far: f32) -> M4 {
    let ys = 1.0 / (fov_y * 0.5).tan();
    let xs = ys / aspect;
    let q = far / (far - near);
    [xs, 0., 0., 0., 0., ys, 0., 0., 0., 0., q, 1., 0., 0., -q * near, 0.]
}

unsafe fn create_texture(dev: &ID3D12Device, list: &ID3D12GraphicsCommandList, t: &Tex) -> Result<(ID3D12Resource, ID3D12Resource)> {
    unsafe {
        let format = DXGI_FORMAT(t.format as i32);
        let block = matches!(t.format, 71 | 77 | 83 | 98);
        if block && (t.width % 4 != 0 || t.height % 4 != 0) || t.mips.is_empty() {
            return Err(Error::from(E_INVALIDARG));
        }
        let mips = t.mips.len() as u16;
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: t.width as u64,
            Height: t.height,
            DepthOrArraySize: 1,
            MipLevels: mips,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            ..Default::default()
        };
        let mut res: Option<ID3D12Resource> = None;
        dev.CreateCommittedResource(&heap(D3D12_HEAP_TYPE_DEFAULT), D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut res)?;
        let res = res.ok_or_else(|| Error::from(E_FAIL))?;
        let mut layouts = vec![D3D12_PLACED_SUBRESOURCE_FOOTPRINT::default(); mips as usize];
        let mut rows = vec![0u32; mips as usize];
        let mut row_bytes = vec![0u64; mips as usize];
        let mut total = 0u64;
        dev.GetCopyableFootprints(&desc, 0, mips as u32, 0, Some(layouts.as_mut_ptr()), Some(rows.as_mut_ptr()), Some(row_bytes.as_mut_ptr()), Some(&mut total));
        let up = upload_buffer(dev, total)?;
        let mut p = std::ptr::null_mut();
        up.Map(0, None, Some(&mut p))?;
        let base = p as *mut u8;
        for m in 0..mips as usize {
            let src = &t.mips[m];
            let l = &layouts[m];
            let rb = row_bytes[m] as usize;
            for r in 0..rows[m] as usize {
                let so = r * rb;
                if so + rb > src.len() {
                    break;
                }
                std::ptr::copy_nonoverlapping(src.as_ptr().add(so), base.add(l.Offset as usize + r * l.Footprint.RowPitch as usize), rb);
            }
        }
        up.Unmap(0, None);
        for m in 0..mips as usize {
            let dst = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&res),
                Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: m as u32 },
            };
            let src = D3D12_TEXTURE_COPY_LOCATION {
                pResource: std::mem::transmute_copy(&up),
                Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { PlacedFootprint: layouts[m] },
            };
            list.CopyTextureRegion(&dst, 0, 0, 0, &src, None);
        }
        list.ResourceBarrier(&[transition(&res, D3D12_RESOURCE_STATE_COPY_DEST, D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE)]);
        Ok((res, up))
    }
}
