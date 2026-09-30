#![allow(clippy::manual_clamp)]

pub mod buffer;
pub mod cli;
pub mod dispatch;
#[cfg(not(target_os = "wasi"))]
mod git;
#[cfg(target_os = "wasi")]
mod git_wasi;
#[cfg(target_os = "wasi")]
mod inotify_wasi;
#[cfg(target_os = "wasi")]
pub mod wasi_process;
pub mod plugin;
#[cfg(not(target_os = "wasi"))]
pub mod terminal;
#[cfg(target_os = "wasi")]
pub mod terminal_wasi;
pub mod watcher;

use std::{
    io::{BufReader, stdin, stdout},
    process::exit,
    sync::Arc,
    thread,
};

use anyhow::{Result, anyhow};
use clap::Parser;
use dispatch::Dispatcher;
use lapce_core::{directory::Directory, meta};
use lapce_rpc::{
    RpcMessage,
    core::{CoreRpc, CoreRpcHandler},
    file::PathObject,
    proxy::{ProxyMessage, ProxyNotification, ProxyRpcHandler},
    stdio::stdio_transport,
};
use tracing::error;

#[derive(Parser)]
#[clap(name = "Lapce-proxy")]
#[clap(version = meta::VERSION)]
struct Cli {
    #[clap(short, long, action, hide = true)]
    proxy: bool,

    /// Paths to file(s) and/or folder(s) to open.
    /// When path is a file (that exists or not),
    /// it accepts `path:line:column` syntax
    /// to specify line and column at which it should open the file
    #[clap(value_parser = cli::parse_file_line_column)]
    #[clap(value_hint = clap::ValueHint::AnyPath)]
    paths: Vec<PathObject>,
}

pub fn mainloop() {
    let cli = Cli::parse();
    if !cli.proxy {
        if let Err(e) = cli::try_open_in_existing_process(&cli.paths) {
            error!("failed to open path(s): {e}");
        };
        exit(1);
    }
    let core_rpc = CoreRpcHandler::new();
    let proxy_rpc = ProxyRpcHandler::new();
    let mut dispatcher = Dispatcher::new(core_rpc.clone(), proxy_rpc.clone());

    let (writer_tx, writer_rx) = crossbeam_channel::unbounded();
    let (reader_tx, reader_rx) = crossbeam_channel::unbounded();
    stdio_transport(stdout(), writer_rx, BufReader::new(stdin()), reader_tx);

    let local_core_rpc = core_rpc.clone();
    let local_writer_tx = writer_tx.clone();
    thread::spawn(move || {
        for msg in local_core_rpc.rx() {
            match msg {
                CoreRpc::Request(id, rpc) => {
                    if let Err(err) =
                        local_writer_tx.send(RpcMessage::Request(id, rpc))
                    {
                        tracing::error!("{:?}", err);
                    }
                }
                CoreRpc::Notification(rpc) => {
                    if let Err(err) =
                        local_writer_tx.send(RpcMessage::Notification(rpc))
                    {
                        tracing::error!("{:?}", err);
                    }
                }
                CoreRpc::Shutdown => {
                    return;
                }
            }
        }
    });

    let local_proxy_rpc = proxy_rpc.clone();
    let writer_tx = Arc::new(writer_tx);
    thread::spawn(move || {
        for msg in reader_rx {
            match msg {
                RpcMessage::Request(id, req) => {
                    let writer_tx = writer_tx.clone();
                    local_proxy_rpc.request_async(req, move |result| match result {
                        Ok(resp) => {
                            if let Err(err) =
                                writer_tx.send(RpcMessage::Response(id, resp))
                            {
                                tracing::error!("{:?}", err);
                            }
                        }
                        Err(e) => {
                            if let Err(err) =
                                writer_tx.send(RpcMessage::Error(id, e))
                            {
                                tracing::error!("{:?}", err);
                            }
                        }
                    });
                }
                RpcMessage::Notification(n) => {
                    local_proxy_rpc.notification(n);
                }
                RpcMessage::Response(id, resp) => {
                    core_rpc.handle_response(id, Ok(resp));
                }
                RpcMessage::Error(id, err) => {
                    core_rpc.handle_response(id, Err(err));
                }
            }
        }
        local_proxy_rpc.shutdown();
    });

    let local_proxy_rpc = proxy_rpc.clone();
    std::thread::spawn(move || {
        if let Err(err) = listen_local_socket(local_proxy_rpc) {
            tracing::error!("{:?}", err);
        }
    });
    if let Err(err) = register_lapce_path() {
        tracing::error!("{:?}", err);
    }

    proxy_rpc.mainloop(&mut dispatcher);
}

pub fn register_lapce_path() -> Result<()> {
    let exedir = std::env::current_exe()?
        .parent()
        .ok_or(anyhow!("can't get parent dir of exe"))?
        .canonicalize()?;

    let current_path = std::env::var("PATH")?;
    let paths = std::env::split_paths(&current_path);
    for path in paths {
        if exedir == path.canonicalize()? {
            return Ok(());
        }
    }
    let paths = std::env::split_paths(&current_path);
    let paths = std::env::join_paths(std::iter::once(exedir).chain(paths))?;

    unsafe {
        std::env::set_var("PATH", paths);
    }

    Ok(())
}

/// WASI（preview1）に Unix ドメインソケットは無い。2 つ目の Lapce は
/// 前のものへ繋がず、自分で窓を開く
#[cfg(target_os = "wasi")]
fn listen_local_socket(_proxy_rpc: ProxyRpcHandler) -> Result<()> {
    Err(anyhow!("local sockets are not available on WASI"))
}

#[cfg(not(target_os = "wasi"))]
fn listen_local_socket(proxy_rpc: ProxyRpcHandler) -> Result<()> {
    let local_socket = Directory::local_socket()
        .ok_or_else(|| anyhow!("can't get local socket folder"))?;
    if let Err(err) = std::fs::remove_file(&local_socket) {
        tracing::error!("{:?}", err);
    }
    let socket =
        interprocess::local_socket::LocalSocketListener::bind(local_socket)?;
    for stream in socket.incoming().flatten() {
        let mut reader = BufReader::new(stream);
        let proxy_rpc = proxy_rpc.clone();
        thread::spawn(move || -> Result<()> {
            loop {
                let msg: Option<ProxyMessage> =
                    lapce_rpc::stdio::read_msg(&mut reader)?;
                if let Some(RpcMessage::Notification(
                    ProxyNotification::OpenPaths { paths },
                )) = msg
                {
                    proxy_rpc.notification(ProxyNotification::OpenPaths { paths });
                }
            }
        });
    }
    Ok(())
}

/// プラグインの取得や更新の確認に使う。WASI 版はまだ HTTP を持たないので、
/// 必ず失敗する（→ [`wasi_http::Response`]）
#[cfg(target_os = "wasi")]
pub fn get_url<T: std::fmt::Display>(
    url: T,
    _user_agent: Option<&str>,
) -> Result<wasi_http::Response> {
    Err(anyhow!("cannot fetch {url} on WASI yet"))
}

/// reqwest の `Response` のうち、Lapce が使うところだけを持つ型。
/// **値は作られない**（`get_url` が必ず失敗する）ので、中身は空の enum にしてある
#[cfg(target_os = "wasi")]
pub mod wasi_http {
    use anyhow::Result;

    pub enum Response {}

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct StatusCode(pub u16);

    impl StatusCode {
        pub fn is_success(&self) -> bool {
            (200..300).contains(&self.0)
        }
    }

    impl PartialEq<u16> for StatusCode {
        fn eq(&self, other: &u16) -> bool {
            self.0 == *other
        }
    }

    impl std::fmt::Display for StatusCode {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "{}", self.0)
        }
    }

    impl Response {
        pub fn status(&self) -> StatusCode {
            match *self {}
        }

        pub fn text(self) -> Result<String> {
            match self {}
        }

        pub fn bytes(self) -> Result<Vec<u8>> {
            match self {}
        }

        pub fn json<T: serde::de::DeserializeOwned>(self) -> Result<T> {
            match self {}
        }

        pub fn copy_to<W: std::io::Write + ?Sized>(&mut self, _out: &mut W) -> Result<u64> {
            match *self {}
        }
    }

    impl std::io::Read for Response {
        fn read(&mut self, _buf: &mut [u8]) -> std::io::Result<usize> {
            match *self {}
        }
    }

    impl Response {
        pub fn headers(&self) -> &Headers {
            match *self {}
        }
    }

    pub enum Headers {}

    impl Headers {
        pub fn get(&self, _name: &str) -> Option<&HeaderValue> {
            match *self {}
        }
    }

    pub enum HeaderValue {}

    impl HeaderValue {
        pub fn to_str(&self) -> Result<&str> {
            match *self {}
        }
    }
}

#[cfg(not(target_os = "wasi"))]
pub fn get_url<T: reqwest::IntoUrl + Clone>(
    url: T,
    user_agent: Option<&str>,
) -> Result<reqwest::blocking::Response> {
    let mut builder = if let Ok(proxy) = std::env::var("https_proxy") {
        let proxy = reqwest::Proxy::all(proxy)?;
        reqwest::blocking::Client::builder()
            .proxy(proxy)
            .timeout(std::time::Duration::from_secs(10))
    } else {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
    };
    if let Some(user_agent) = user_agent {
        builder = builder.user_agent(user_agent);
    }
    let client = builder.build()?;
    let mut try_time = 0;
    loop {
        let rs = client.get(url.clone()).send();
        if rs.is_ok() || try_time > 3 {
            return Ok(rs?);
        } else {
            try_time += 1;
        }
    }
}
