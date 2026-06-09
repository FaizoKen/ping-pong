// Dev helper: signs a request body the way Discord would, so we can smoke-test
// the server end to end. Not part of the deployed image.
//
//   echo -n '<body>' | cargo run --example sign -- <seed_hex_32_bytes> <timestamp>
//
// Prints: "<public_key_hex> <signature_hex>" on stdout.
use ed25519_dalek::{Signer, SigningKey};
use std::io::Read;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seed: [u8; 32] = hex::decode(&args[1])
        .expect("seed must be hex")
        .try_into()
        .expect("seed must be 32 bytes");
    let timestamp = &args[2];

    let signing_key = SigningKey::from_bytes(&seed);
    let verifying_key = signing_key.verifying_key();

    let mut body = Vec::new();
    std::io::stdin().read_to_end(&mut body).unwrap();

    let mut message = Vec::new();
    message.extend_from_slice(timestamp.as_bytes());
    message.extend_from_slice(&body);
    let signature = signing_key.sign(&message);

    println!(
        "{} {}",
        hex::encode(verifying_key.to_bytes()),
        hex::encode(signature.to_bytes())
    );
}
