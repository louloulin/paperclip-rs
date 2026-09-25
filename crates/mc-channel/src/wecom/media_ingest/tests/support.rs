//! `media_ingest::tests` 的脚手架：一个脚本化的媒体服务端。

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// 一条脚本化的响应。
pub struct MediaScript {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl MediaScript {
    /// 200 + 这份 body。
    pub fn ok(body: &[u8]) -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: body.to_vec(),
        }
    }

    /// 一个非 2xx。
    pub fn status(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// 加一个响应头。
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }
}

/// 起一个脚本化的 HTTP/1.1 服务端，返回端口。
pub async fn spawn_media_stub(scripts: Vec<MediaScript>) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        for script in scripts {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        buffer.extend_from_slice(&chunk[..read]);
                        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                            break;
                        }
                    }
                }
            }
            let mut out = format!("HTTP/1.1 {} X\r\n", script.status);
            for (name, value) in &script.headers {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("{name}: {value}\r\n"));
            }
            let _ = std::fmt::Write::write_fmt(
                &mut out,
                format_args!("content-length: {}\r\n", script.body.len()),
            );
            out.push_str("connection: close\r\n\r\n");
            socket.write_all(out.as_bytes()).await.ok();
            socket.write_all(&script.body).await.ok();
            socket.flush().await.ok();
            socket.shutdown().await.ok();
        }
    });
    port
}
