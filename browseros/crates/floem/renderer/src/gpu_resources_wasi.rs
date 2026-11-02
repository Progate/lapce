//! WASI には GPU が無い。**型だけを残して、取得は必ず失敗させる。**
//!
//! BrowserOS の中のプロセスから WebGPU は呼べない（WebGPU はホスト側、つまり
//! ブラウザーのページにしか無い）。floem は GPU が取れなければ tiny-skia で描くので、
//! 型が揃っていれば描画の経路はそのまま使える。

use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;

use winit::window::{Window, WindowId};

/// wgpu の代役。floem が名前で参照するものだけを置く
pub mod wgpu {
    use std::marker::PhantomData;

    /// 作られることの無い描画先
    pub struct Surface<'window>(std::convert::Infallible, PhantomData<&'window ()>);

    /// 要求する GPU の機能。GPU が無いので中身も無い
    #[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Features;
}

/// 取得できることの無い GPU の資源
#[derive(Debug, Clone)]
pub struct GpuResources(std::convert::Infallible);

#[derive(Debug)]
pub enum GpuResourceError {
    /// この環境に GPU は無い
    Unsupported,
}

impl std::fmt::Display for GpuResourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("there is no GPU in WASI")
    }
}

impl std::error::Error for GpuResourceError {}

impl GpuResources {
    pub fn request<F: Fn(WindowId) + 'static>(
        _on_result: F,
        _required_features: wgpu::Features,
        _window: Arc<dyn Window>,
    ) -> Receiver<Result<(Self, wgpu::Surface<'static>), GpuResourceError>> {
        let (tx, rx) = sync_channel(1);
        let _ = tx.send(Err(GpuResourceError::Unsupported));
        rx
    }
}
