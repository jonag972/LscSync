//! Integration test: a fake LSC panel on loopback speaks the Tuya v3.5 local
//! protocol (6699 frames, AES-128-GCM, session-key handshake) and checks the
//! DP commands `Lsc` sends: power, mode, color, music and scene.

use std::time::Duration;

use aes_gcm::aead::consts::U12;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes128Gcm, Nonce};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use lscsync_core::{Hsv, protocol};
use lscsync_engine::tuya::{Lsc, LscConfig};

const LOCAL_KEY: &[u8; 16] = b"0123456789abcdef";
const DEVICE_ID: &str = "bf0123456789abcdef0123";
/// A port nothing else uses, so the test never collides with a real panel.
const PANEL_PORT: u16 = 16668;

const CMD_SESS_KEY_NEG_START: u32 = 0x03;
const CMD_SESS_KEY_NEG_RESP: u32 = 0x04;
const CMD_SESS_KEY_NEG_FINISH: u32 = 0x05;
const CMD_HEARTBEAT: u32 = 0x09;
const CMD_CONTROL_NEW: u32 = 0x0d;

type HmacSha256 = Hmac<Sha256>;

fn gcm_encrypt(key: &[u8; 16], iv: &[u8; 12], plaintext: &[u8], aad: &[u8]) -> Vec<u8> {
    let cipher = Aes128Gcm::new(key.into());
    let nonce: &Nonce<U12> = iv.into();
    cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("gcm encrypt")
}

fn gcm_decrypt(key: &[u8; 16], iv: &[u8; 12], ct: &[u8], aad: &[u8]) -> Option<Vec<u8>> {
    let cipher = Aes128Gcm::new(key.into());
    let nonce: &Nonce<U12> = iv.into();
    cipher.decrypt(nonce, Payload { msg: ct, aad }).ok()
}

/// A 6699 frame: 18-byte header, IV, GCM ciphertext+tag, suffix. The AAD is the
/// header without its 4-byte prefix.
fn pack_6699(seqno: u32, cmd: u32, plaintext: &[u8], key: &[u8; 16], iv: &[u8; 12]) -> Vec<u8> {
    let mut frame = Vec::new();
    frame.extend_from_slice(&0x0000_6699_u32.to_be_bytes());
    frame.extend_from_slice(&0_u16.to_be_bytes());
    frame.extend_from_slice(&seqno.to_be_bytes());
    frame.extend_from_slice(&cmd.to_be_bytes());
    frame.extend_from_slice(&((12 + plaintext.len() + 16) as u32).to_be_bytes());
    let ct = gcm_encrypt(key, iv, plaintext, &frame[4..]);
    frame.extend_from_slice(iv);
    frame.extend_from_slice(&ct);
    frame.extend_from_slice(&0x0000_9966_u32.to_be_bytes());
    frame
}

/// Reads one whole 6699 frame and returns (seqno, cmd, GCM plaintext).
async fn read_message(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    key: &[u8; 16],
) -> Option<(u32, u32, Vec<u8>)> {
    loop {
        if buf.len() >= 18 {
            let prefix = u32::from_be_bytes(buf[..4].try_into().unwrap());
            assert_eq!(prefix, 0x0000_6699, "6699 frame prefix");
            let len = u32::from_be_bytes(buf[14..18].try_into().unwrap()) as usize;
            let frame_len = 18 + len + 4;
            if buf.len() >= frame_len {
                let frame: Vec<u8> = buf.drain(..frame_len).collect();
                let seqno = u32::from_be_bytes(frame[6..10].try_into().unwrap());
                let cmd = u32::from_be_bytes(frame[10..14].try_into().unwrap());
                let iv: [u8; 12] = frame[18..30].try_into().unwrap();
                let aad = frame[4..18].to_vec();
                let ct = frame[30..frame_len - 4].to_vec();
                let plaintext = gcm_decrypt(key, &iv, &ct, &aad).expect("frame authenticates");
                return Some((seqno, cmd, plaintext));
            }
        }
        if stream.read_buf(buf).await.ok()? == 0 {
            return None;
        }
    }
}

/// The fake panel: answers the session-key handshake, then records every dps
/// object the client sends until the connection closes.
async fn fake_panel(
    listener: TcpListener,
    dps_tx: tokio::sync::mpsc::UnboundedSender<serde_json::Value>,
) {
    let (mut stream, _) = listener.accept().await.expect("accept panel connection");
    let mut buf: Vec<u8> = Vec::new();

    // Handshake step 1: the client's 16-byte nonce (session-key commands carry
    // no "3.5" header), framed with the local key.
    let (_seq, cmd, payload) = read_message(&mut stream, &mut buf, LOCAL_KEY)
        .await
        .expect("handshake start");
    assert_eq!(cmd, CMD_SESS_KEY_NEG_START);
    let local_nonce: [u8; 16] = payload[..].try_into().expect("16-byte nonce");

    // Handshake step 2: remote nonce + HMAC-SHA256(local key, local nonce),
    // behind a 4-byte retcode (0) inside the GCM plaintext.
    let remote_nonce = [0x42_u8; 16];
    let mut mac = HmacSha256::new_from_slice(LOCAL_KEY).expect("hmac key");
    mac.update(&local_nonce);
    let mut resp = 0_u32.to_be_bytes().to_vec();
    resp.extend_from_slice(&remote_nonce);
    resp.extend_from_slice(&mac.finalize().into_bytes());
    stream
        .write_all(&pack_6699(
            1,
            CMD_SESS_KEY_NEG_RESP,
            &resp,
            LOCAL_KEY,
            &[0x07; 12],
        ))
        .await
        .expect("send handshake response");

    // Handshake step 3: HMAC-SHA256(local key, remote nonce), still framed with
    // the local key.
    let (_seq, cmd, finish) = read_message(&mut stream, &mut buf, LOCAL_KEY)
        .await
        .expect("handshake finish");
    assert_eq!(cmd, CMD_SESS_KEY_NEG_FINISH);
    let mut mac = HmacSha256::new_from_slice(LOCAL_KEY).expect("hmac key");
    mac.update(&remote_nonce);
    mac.verify_slice(&finish).expect("finish hmac");

    // Session key: first 16 bytes of GCM(local key, iv = local nonce[..12], xor
    // of the nonces), tag excluded.
    let mut xor = [0_u8; 16];
    for (x, (&l, &r)) in xor.iter_mut().zip(local_nonce.iter().zip(&remote_nonce)) {
        *x = l ^ r;
    }
    let session_iv: [u8; 12] = local_nonce[..12].try_into().unwrap();
    let ct = gcm_encrypt(LOCAL_KEY, &session_iv, &xor, &[]);
    let session_key: [u8; 16] = ct[..16].try_into().unwrap();

    // Commands arrive under the session key, as "3.5" header + JSON.
    while let Some((_seq, cmd, plaintext)) = read_message(&mut stream, &mut buf, &session_key).await
    {
        if cmd == CMD_HEARTBEAT {
            continue;
        }
        assert_eq!(cmd, CMD_CONTROL_NEW, "v3.5 control is remapped to 0x0d");
        assert_eq!(&plaintext[..3], b"3.5", "version header");
        let message: serde_json::Value =
            serde_json::from_slice(&plaintext[15..]).expect("command json");
        dps_tx
            .send(message["data"]["dps"].clone())
            .expect("test still listening");
    }
}

#[tokio::test]
async fn lsc_sends_the_expected_dp_commands_to_a_v3_5_panel() {
    let listener = TcpListener::bind(("127.0.0.1", PANEL_PORT))
        .await
        .expect("bind the fake panel");
    let (dps_tx, mut dps_rx) = tokio::sync::mpsc::unbounded_channel();
    let panel = tokio::spawn(fake_panel(listener, dps_tx));

    let config = LscConfig {
        device_id: DEVICE_ID.to_string(),
        local_key: String::from_utf8(LOCAL_KEY.to_vec()).unwrap(),
        ip: "127.0.0.1".to_string(),
        port: PANEL_PORT,
    };
    let lsc = Lsc::connect(&config, Duration::from_secs(10))
        .await
        .expect("connect to the fake panel");

    let red = Hsv::new(0.0, 1.0, 1.0);
    let blue = Hsv::new(2.0 / 3.0, 1.0, 1.0);
    let panels = [red, red, blue, blue, red, blue];
    lsc.send_color(red).await.expect("send color");
    lsc.send_direct(blue).await.expect("send direct color");
    lsc.send_scene_panels(&panels).await.expect("send scene");
    lsc.restore_white().await.expect("restore white");
    lsc.close().await;

    panel.await.expect("fake panel failed");
    let mut received = Vec::new();
    while let Some(dps) = dps_rx.recv().await {
        received.push(dps);
    }

    let expected = [
        (protocol::DP_POWER, serde_json::json!(true)),
        (protocol::DP_MODE, serde_json::json!(protocol::MODE_COLOR)),
        (
            protocol::DP_COLOR,
            serde_json::json!(protocol::dp_color(red)),
        ),
        (protocol::DP_MODE, serde_json::json!(protocol::MODE_MUSIC)),
        (
            protocol::DP_MUSIC,
            serde_json::json!(protocol::dp_music(blue)),
        ),
        (protocol::DP_MODE, serde_json::json!(protocol::MODE_SCENE)),
        (
            protocol::DP_SCENE,
            serde_json::json!(protocol::dp_scene_panels(&panels)),
        ),
        (protocol::DP_MODE, serde_json::json!(protocol::MODE_WHITE)),
    ];
    assert_eq!(received.len(), expected.len(), "one dps per command");
    for (got, (dp, value)) in received.iter().zip(&expected) {
        let object = got.as_object().expect("dps is an object");
        assert_eq!(object.len(), 1, "one dp per command");
        assert_eq!(object.get(*dp), Some(value), "dp {dp}");
    }
}
