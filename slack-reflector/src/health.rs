use std::net::SocketAddr;
use tokio::io::AsyncWriteExt;
use tracing::info;

/// Simple HTTP health check endpoint
pub async fn run_health_server(port: u16) {
    if port == 0 {
        return;
    }

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("health check listening on {}", addr);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("failed to bind health port {}: {}", port, e);
            return;
        }
    };

    loop {
        if let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"ok\",\"service\":\"slack-reflector\"}\n";
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    }
}
