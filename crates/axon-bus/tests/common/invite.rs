//! Inspect and corrupt public invitation blobs; never implement the peer protocol here.
use serde_json::Value;
const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
pub fn decode(invite: &str) -> Value {
    let encoded = invite
        .strip_prefix("axon1:")
        .expect("versioned invitation prefix");
    let mut bytes = Vec::new();
    let (mut bits, mut available) = (0u32, 0);
    for c in encoded.bytes() {
        let digit = ALPHABET
            .iter()
            .position(|b| *b == c)
            .expect("base64url without padding");
        bits = (bits << 6) | digit as u32;
        available += 6;
        if available >= 8 {
            available -= 8;
            bytes.push((bits >> available) as u8);
        }
    }
    serde_json::from_slice(&bytes).unwrap()
}
pub fn encode(value: &Value) -> String {
    let mut encoded = String::from("axon1:");
    let (mut bits, mut available) = (0u32, 0);
    for byte in serde_json::to_vec(value).unwrap() {
        bits = (bits << 8) | u32::from(byte);
        available += 8;
        while available >= 6 {
            available -= 6;
            encoded.push(ALPHABET[((bits >> available) & 63) as usize] as char);
        }
    }
    if available > 0 {
        encoded.push(ALPHABET[((bits << (6 - available)) & 63) as usize] as char);
    }
    encoded
}
