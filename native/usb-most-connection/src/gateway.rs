//! Mutual authentication of a local hop. The secret is never put on a TCP port
//! which might have been reused after a previous SSH connection disappeared.
use std::io::{self, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
const MAGIC: &[u8; 8] = b"UMGATE2\0";
fn tag(secret: &[u8; 32], role: &[u8], client: &[u8; 32], server: &[u8; 32]) -> ring::hmac::Tag {
    let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret);
    let mut context = ring::hmac::Context::with_key(&key);
    context.update(MAGIC);
    context.update(role);
    context.update(client);
    context.update(server);
    context.sign()
}
fn verify(
    secret: &[u8; 32],
    role: &[u8],
    client: &[u8; 32],
    server: &[u8; 32],
    got: &[u8],
) -> io::Result<()> {
    use subtle::ConstantTimeEq;
    if bool::from(tag(secret, role, client, server).as_ref().ct_eq(got)) {
        Ok(())
    } else {
        Err(io::ErrorKind::PermissionDenied.into())
    }
}
fn random() -> io::Result<[u8; 32]> {
    crate::security::random().map_err(|_| io::ErrorKind::Other.into())
}
pub fn client_sync(stream: &mut (impl Read + Write), secret: &[u8; 32]) -> io::Result<()> {
    let client = random()?;
    stream.write_all(MAGIC)?;
    stream.write_all(&client)?;
    let mut server = [0; 32];
    let mut got = [0; 32];
    stream.read_exact(&mut server)?;
    stream.read_exact(&mut got)?;
    verify(secret, b"server", &client, &server, &got)?;
    stream.write_all(tag(secret, b"client", &client, &server).as_ref())
}
pub async fn client(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    secret: &[u8; 32],
) -> io::Result<()> {
    let client = random()?;
    stream.write_all(MAGIC).await?;
    stream.write_all(&client).await?;
    let mut server = [0; 32];
    let mut got = [0; 32];
    stream.read_exact(&mut server).await?;
    stream.read_exact(&mut got).await?;
    verify(secret, b"server", &client, &server, &got)?;
    stream
        .write_all(tag(secret, b"client", &client, &server).as_ref())
        .await
}
pub async fn server(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    secret: &[u8; 32],
) -> io::Result<()> {
    let mut magic = [0; 8];
    let mut client = [0; 32];
    stream.read_exact(&mut magic).await?;
    if &magic != MAGIC {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    stream.read_exact(&mut client).await?;
    let server = random()?;
    stream.write_all(&server).await?;
    stream
        .write_all(tag(secret, b"server", &client, &server).as_ref())
        .await?;
    let mut got = [0; 32];
    stream.read_exact(&mut got).await?;
    verify(secret, b"client", &client, &server, &got)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn mutual_authentication_and_wrong_key_rejection() {
        for wrong in [false, true] {
            let (mut a, mut b) = tokio::io::duplex(128);
            let task = tokio::spawn(async move { server(&mut a, &[1; 32]).await });
            let key = if wrong { [2; 32] } else { [1; 32] };
            assert_eq!(client(&mut b, &key).await.is_err(), wrong);
            drop(b);
            assert_eq!(task.await.unwrap().is_err(), wrong);
        }
    }
    #[tokio::test]
    async fn replayed_client_response_does_not_authenticate_a_new_challenge() {
        let (mut a, mut b) = tokio::io::duplex(128);
        let task = tokio::spawn(async move { server(&mut a, &[3; 32]).await });
        b.write_all(MAGIC).await.unwrap();
        b.write_all(&[1; 32]).await.unwrap();
        let mut challenge = [0; 64];
        b.read_exact(&mut challenge).await.unwrap();
        b.write_all(tag(&[3; 32], b"client", &[1; 32], &[0; 32]).as_ref())
            .await
            .unwrap();
        assert!(task.await.unwrap().is_err());
    }
}
