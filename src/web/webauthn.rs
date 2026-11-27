use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Format,
    Type,
    Challenge,
    Origin,
    RpId,
    Flags,
    Alg,
    Signature,
    Counter,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::Format => "malformed passkey data",
            Error::Type => "wrong ceremony type",
            Error::Challenge => "wrong challenge",
            Error::Origin => "wrong origin",
            Error::RpId => "wrong relying party",
            Error::Flags => "the passkey was not user verified",
            Error::Alg => "unsupported key type",
            Error::Signature => "bad signature",
            Error::Counter => "the signature counter went back",
        })
    }
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64_encode(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 4 / 3 + 2);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        if c.len() > 1 {
            out.push(ALPHABET[(n >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            out.push(ALPHABET[n as usize & 63] as char);
        }
    }
    out
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for ch in s.bytes() {
        let v = ALPHABET.iter().position(|a| *a == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cbor {
    Uint(u64),
    Nint(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Other,
}

const MAX_DEPTH: usize = 8;
const MAX_ITEMS: u64 = 64;

fn head(b: &[u8]) -> Option<(u8, u64, usize)> {
    let first = *b.first()?;
    let (major, info) = (first >> 5, first & 31);
    let (val, used) = match info {
        0..=23 => (info as u64, 1),
        24 => (*b.get(1)? as u64, 2),
        25 => (u16::from_be_bytes(b.get(1..3)?.try_into().ok()?) as u64, 3),
        26 => (u32::from_be_bytes(b.get(1..5)?.try_into().ok()?) as u64, 5),
        27 => (u64::from_be_bytes(b.get(1..9)?.try_into().ok()?), 9),
        _ => return None,
    };
    Some((major, val, used))
}

pub fn cbor_parse(b: &[u8]) -> Option<(Cbor, usize)> {
    parse_at(b, 0)
}

fn parse_at(b: &[u8], depth: usize) -> Option<(Cbor, usize)> {
    if depth > MAX_DEPTH {
        return None;
    }
    let (major, val, used) = head(b)?;
    match major {
        0 => Some((Cbor::Uint(val), used)),
        1 => Some((Cbor::Nint(-1 - i64::try_from(val).ok()?), used)),
        2 | 3 => {
            let n = usize::try_from(val).ok()?;
            let end = used.checked_add(n)?;
            let body = b.get(used..end)?;
            if major == 2 {
                Some((Cbor::Bytes(body.to_vec()), end))
            } else {
                Some((Cbor::Text(std::str::from_utf8(body).ok()?.to_string()), end))
            }
        }
        4 => {
            if val > MAX_ITEMS {
                return None;
            }
            let mut at = used;
            let mut v = vec![];
            for _ in 0..val {
                let (x, n) = parse_at(b.get(at..)?, depth + 1)?;
                v.push(x);
                at += n;
            }
            Some((Cbor::Array(v), at))
        }
        5 => {
            if val > MAX_ITEMS {
                return None;
            }
            let mut at = used;
            let mut v = vec![];
            for _ in 0..val {
                let (k, n) = parse_at(b.get(at..)?, depth + 1)?;
                at += n;
                let (x, n) = parse_at(b.get(at..)?, depth + 1)?;
                at += n;
                v.push((k, x));
            }
            Some((Cbor::Map(v), at))
        }
        6 => {
            let (x, n) = parse_at(b.get(used..)?, depth + 1)?;
            Some((x, used + n))
        }
        _ => Some((Cbor::Other, used)),
    }
}

impl Cbor {
    fn get_int(&self, key: i64) -> Option<&Cbor> {
        let Cbor::Map(m) = self else {
            return None;
        };
        m.iter().find_map(|(k, v)| match k {
            Cbor::Uint(u) if i64::try_from(*u).ok() == Some(key) => Some(v),
            Cbor::Nint(n) if *n == key => Some(v),
            _ => None,
        })
    }

    fn get_text(&self, key: &str) -> Option<&Cbor> {
        let Cbor::Map(m) = self else {
            return None;
        };
        m.iter().find_map(|(k, v)| match k {
            Cbor::Text(t) if t == key => Some(v),
            _ => None,
        })
    }

    fn int(&self) -> Option<i64> {
        match self {
            Cbor::Uint(u) => i64::try_from(*u).ok(),
            Cbor::Nint(n) => Some(*n),
            _ => None,
        }
    }

    fn bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }
}

pub fn es256_key(cose: &Cbor) -> Option<[u8; 65]> {
    if cose.get_int(1)?.int()? != 2
        || cose.get_int(3)?.int()? != -7
        || cose.get_int(-1)?.int()? != 1
    {
        return None;
    }
    let x = cose.get_int(-2)?.bytes()?;
    let y = cose.get_int(-3)?.bytes()?;
    if x.len() != 32 || y.len() != 32 {
        return None;
    }
    let mut k = [0u8; 65];
    k[0] = 4;
    k[1..33].copy_from_slice(x);
    k[33..].copy_from_slice(y);
    Some(k)
}

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
pub const FLAG_AT: u8 = 0x40;

#[derive(Debug, Clone, PartialEq)]
pub struct Registered {
    pub id: Vec<u8>,
    pub key: [u8; 65],
    pub counter: u32,
}

struct Client {
    challenge: Vec<u8>,
    origin: String,
}

fn client_data(json: &[u8], want_type: &str) -> Result<Client, Error> {
    let v: Value = serde_json::from_slice(json).map_err(|_| Error::Format)?;
    if v["type"].as_str() != Some(want_type) {
        return Err(Error::Type);
    }
    if v["crossOrigin"].as_bool() == Some(true) {
        return Err(Error::Origin);
    }
    let challenge = v["challenge"]
        .as_str()
        .and_then(b64_decode)
        .ok_or(Error::Format)?;
    let origin = v["origin"].as_str().ok_or(Error::Format)?.to_string();
    Ok(Client { challenge, origin })
}

fn same(a: &[u8], b: &[u8]) -> bool {
    super::auth::same(a, b)
}

fn check_client(c: &Client, challenge: &[u8], origin: &str) -> Result<(), Error> {
    if !same(&c.challenge, challenge) {
        return Err(Error::Challenge);
    }
    if !same(c.origin.as_bytes(), origin.as_bytes()) {
        return Err(Error::Origin);
    }
    Ok(())
}

fn rp_hash_ok(auth_data: &[u8], rp_id: &str) -> Result<(), Error> {
    let want = Sha256::digest(rp_id.as_bytes());
    if !same(auth_data.get(..32).ok_or(Error::Format)?, &want) {
        return Err(Error::RpId);
    }
    Ok(())
}

pub fn verify_registration(
    client_data_json: &[u8],
    attestation_object: &[u8],
    challenge: &[u8],
    origin: &str,
    rp_id: &str,
) -> Result<Registered, Error> {
    let c = client_data(client_data_json, "webauthn.create")?;
    check_client(&c, challenge, origin)?;
    let (att, _) = cbor_parse(attestation_object).ok_or(Error::Format)?;
    let auth = att
        .get_text("authData")
        .and_then(Cbor::bytes)
        .ok_or(Error::Format)?;
    rp_hash_ok(auth, rp_id)?;
    let flags = *auth.get(32).ok_or(Error::Format)?;
    if flags & FLAG_UP == 0 || flags & FLAG_UV == 0 {
        return Err(Error::Flags);
    }
    if flags & FLAG_AT == 0 {
        return Err(Error::Format);
    }
    let counter = u32::from_be_bytes(auth.get(33..37).ok_or(Error::Format)?.try_into().unwrap());
    let id_len = u16::from_be_bytes(auth.get(53..55).ok_or(Error::Format)?.try_into().unwrap());
    let id_len = id_len as usize;
    if id_len == 0 || id_len > 1023 {
        return Err(Error::Format);
    }
    let id = auth.get(55..55 + id_len).ok_or(Error::Format)?.to_vec();
    let (cose, _) =
        cbor_parse(auth.get(55 + id_len..).ok_or(Error::Format)?).ok_or(Error::Format)?;
    let key = es256_key(&cose).ok_or(Error::Alg)?;
    Ok(Registered { id, key, counter })
}

pub fn verify_assertion(
    client_data_json: &[u8],
    auth_data: &[u8],
    signature: &[u8],
    challenge: &[u8],
    origin: &str,
    rp_id: &str,
    key: &[u8],
    stored_counter: u32,
) -> Result<u32, Error> {
    let c = client_data(client_data_json, "webauthn.get")?;
    check_client(&c, challenge, origin)?;
    rp_hash_ok(auth_data, rp_id)?;
    let flags = *auth_data.get(32).ok_or(Error::Format)?;
    if flags & FLAG_UP == 0 || flags & FLAG_UV == 0 {
        return Err(Error::Flags);
    }
    let counter = u32::from_be_bytes(
        auth_data
            .get(33..37)
            .ok_or(Error::Format)?
            .try_into()
            .unwrap(),
    );
    let mut msg = auth_data.to_vec();
    msg.extend_from_slice(&Sha256::digest(client_data_json));
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_ASN1, key)
        .verify(&msg, signature)
        .map_err(|_| Error::Signature)?;
    if (stored_counter != 0 || counter != 0) && counter <= stored_counter {
        return Err(Error::Counter);
    }
    Ok(counter)
}

#[cfg(test)]
pub mod soft {
    use super::*;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};

    pub struct Authenticator {
        pair: EcdsaKeyPair,
        pub id: Vec<u8>,
        pub counter: u32,
        pub flags: u8,
    }

    pub fn cbor_head(major: u8, n: u64) -> Vec<u8> {
        let m = major << 5;
        if n < 24 {
            vec![m | n as u8]
        } else if n < 256 {
            vec![m | 24, n as u8]
        } else {
            let mut v = vec![m | 25];
            v.extend_from_slice(&(n as u16).to_be_bytes());
            v
        }
    }

    pub fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        let mut v = cbor_head(2, b.len() as u64);
        v.extend_from_slice(b);
        v
    }

    pub fn cbor_text(s: &str) -> Vec<u8> {
        let mut v = cbor_head(3, s.len() as u64);
        v.extend_from_slice(s.as_bytes());
        v
    }

    pub fn cbor_int(n: i64) -> Vec<u8> {
        if n >= 0 {
            cbor_head(0, n as u64)
        } else {
            cbor_head(1, (-1 - n) as u64)
        }
    }

    impl Authenticator {
        pub fn new(id: &[u8]) -> Authenticator {
            let rng = ring::rand::SystemRandom::new();
            let pkcs8 =
                EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).unwrap();
            let pair =
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
                    .unwrap();
            Authenticator {
                pair,
                id: id.to_vec(),
                counter: 0,
                flags: FLAG_UP | FLAG_UV,
            }
        }

        pub fn public(&self) -> [u8; 65] {
            self.pair.public_key().as_ref().try_into().unwrap()
        }

        fn cose(&self) -> Vec<u8> {
            let k = self.public();
            let mut v = cbor_head(5, 5);
            v.extend(cbor_int(1));
            v.extend(cbor_int(2));
            v.extend(cbor_int(3));
            v.extend(cbor_int(-7));
            v.extend(cbor_int(-1));
            v.extend(cbor_int(1));
            v.extend(cbor_int(-2));
            v.extend(cbor_bytes(&k[1..33]));
            v.extend(cbor_int(-3));
            v.extend(cbor_bytes(&k[33..]));
            v
        }

        fn auth_data(&self, rp_id: &str, attested: bool) -> Vec<u8> {
            let mut d = Sha256::digest(rp_id.as_bytes()).to_vec();
            d.push(self.flags | if attested { FLAG_AT } else { 0 });
            d.extend_from_slice(&self.counter.to_be_bytes());
            if attested {
                d.extend_from_slice(&[0u8; 16]);
                d.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
                d.extend_from_slice(&self.id);
                d.extend(self.cose());
            }
            d
        }

        pub fn client_json(kind: &str, challenge: &[u8], origin: &str) -> Vec<u8> {
            serde_json::json!({"type": kind, "challenge": b64_encode(challenge), "origin": origin})
                .to_string()
                .into_bytes()
        }

        pub fn create(&self, rp_id: &str, client: &[u8]) -> (Vec<u8>, Vec<u8>) {
            let mut att = cbor_head(5, 3);
            att.extend(cbor_text("fmt"));
            att.extend(cbor_text("none"));
            att.extend(cbor_text("attStmt"));
            att.extend(cbor_head(5, 0));
            att.extend(cbor_text("authData"));
            att.extend(cbor_bytes(&self.auth_data(rp_id, true)));
            (client.to_vec(), att)
        }

        pub fn get(&mut self, rp_id: &str, client: &[u8]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
            let auth = self.auth_data(rp_id, false);
            let mut msg = auth.clone();
            msg.extend_from_slice(&Sha256::digest(client));
            let sig = self
                .pair
                .sign(&ring::rand::SystemRandom::new(), &msg)
                .unwrap();
            (client.to_vec(), auth, sig.as_ref().to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::soft::*;
    use super::*;

    const ORIGIN: &str = "https://mac.example.ts.net";
    const RP: &str = "mac.example.ts.net";
    const CH: &[u8] = b"0123456789abcdef0123456789abcdef";

    fn registered() -> (Authenticator, Registered) {
        let a = Authenticator::new(b"cred-1");
        let client = Authenticator::client_json("webauthn.create", CH, ORIGIN);
        let (cd, att) = a.create(RP, &client);
        let r = verify_registration(&cd, &att, CH, ORIGIN, RP).unwrap();
        (a, r)
    }

    #[test]
    fn base64url_round_trips() {
        for n in 0..40usize {
            let v: Vec<u8> = (0..n as u8).map(|x| x.wrapping_mul(37)).collect();
            let e = b64_encode(&v);
            assert!(!e.contains('=') && !e.contains('+') && !e.contains('/'));
            assert_eq!(b64_decode(&e).unwrap(), v);
        }
        assert_eq!(b64_encode(b"\xfb\xff"), "-_8");
        assert!(b64_decode("a").is_none());
        assert!(b64_decode("a*b=").is_none());
    }

    #[test]
    fn registration_stores_the_credential_and_key() {
        let (a, r) = registered();
        assert_eq!(r.id, b"cred-1");
        assert_eq!(r.key, a.public());
        assert_eq!(r.counter, 0);
    }

    #[test]
    fn registration_refuses_wrong_origin_rp_challenge_and_type() {
        let a = Authenticator::new(b"c");
        let ok = Authenticator::client_json("webauthn.create", CH, ORIGIN);
        let (cd, att) = a.create(RP, &ok);
        assert_eq!(
            verify_registration(&cd, &att, CH, "https://evil.example", RP),
            Err(Error::Origin)
        );
        assert_eq!(
            verify_registration(&cd, &att, CH, ORIGIN, "other.example.ts.net"),
            Err(Error::RpId)
        );
        assert_eq!(
            verify_registration(&cd, &att, b"another challenge value 0123456", ORIGIN, RP),
            Err(Error::Challenge)
        );
        let get = Authenticator::client_json("webauthn.get", CH, ORIGIN);
        let (cd, att) = a.create(RP, &get);
        assert_eq!(
            verify_registration(&cd, &att, CH, ORIGIN, RP),
            Err(Error::Type)
        );
        let (_, att) = a.create("evil.example", &ok);
        assert_eq!(
            verify_registration(&ok, &att, CH, ORIGIN, RP),
            Err(Error::RpId)
        );
    }

    #[test]
    fn registration_needs_user_verification_and_an_es256_key() {
        let mut a = Authenticator::new(b"c");
        a.flags = FLAG_UP;
        let ok = Authenticator::client_json("webauthn.create", CH, ORIGIN);
        let (cd, att) = a.create(RP, &ok);
        assert_eq!(
            verify_registration(&cd, &att, CH, ORIGIN, RP),
            Err(Error::Flags)
        );
        assert_eq!(
            verify_registration(&cd, b"\xff\x00", CH, ORIGIN, RP),
            Err(Error::Format)
        );
        assert_eq!(
            verify_registration(b"{", &[], CH, ORIGIN, RP),
            Err(Error::Format)
        );
        let cross = serde_json::json!({"type":"webauthn.create","challenge":b64_encode(CH),"origin":ORIGIN,"crossOrigin":true}).to_string();
        let a = Authenticator::new(b"c");
        let (_, att) = a.create(RP, &ok);
        assert_eq!(
            verify_registration(cross.as_bytes(), &att, CH, ORIGIN, RP),
            Err(Error::Origin)
        );
    }

    #[test]
    fn assertion_verifies_and_advances_the_counter() {
        let (mut a, r) = registered();
        a.counter = 5;
        let client = Authenticator::client_json("webauthn.get", CH, ORIGIN);
        let (cd, ad, sig) = a.get(RP, &client);
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 0),
            Ok(5)
        );
    }

    #[test]
    fn assertion_with_counters_that_stay_zero_is_fine_but_rollback_is_not() {
        let (mut a, r) = registered();
        let client = Authenticator::client_json("webauthn.get", CH, ORIGIN);
        let (cd, ad, sig) = a.get(RP, &client);
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 0),
            Ok(0)
        );
        a.counter = 7;
        let (cd, ad, sig) = a.get(RP, &client);
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 7),
            Err(Error::Counter)
        );
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 9),
            Err(Error::Counter)
        );
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 6),
            Ok(7)
        );
    }

    #[test]
    fn assertion_refuses_bad_signatures_origins_rps_and_challenges() {
        let (mut a, r) = registered();
        let client = Authenticator::client_json("webauthn.get", CH, ORIGIN);
        let (cd, ad, mut sig) = a.get(RP, &client);
        let last = sig.len() - 1;
        sig[last] ^= 1;
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &r.key, 0),
            Err(Error::Signature)
        );
        let (cd, ad, sig) = a.get(RP, &client);
        assert_eq!(
            verify_assertion(
                &cd,
                &ad,
                &sig,
                CH,
                "https://mac.example.ts.net:8443",
                RP,
                &r.key,
                0
            ),
            Err(Error::Origin)
        );
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, "evil.example", &r.key, 0),
            Err(Error::RpId)
        );
        assert_eq!(
            verify_assertion(
                &cd,
                &ad,
                &sig,
                b"zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
                ORIGIN,
                RP,
                &r.key,
                0
            ),
            Err(Error::Challenge)
        );
        let other = Authenticator::new(b"x");
        assert_eq!(
            verify_assertion(&cd, &ad, &sig, CH, ORIGIN, RP, &other.public(), 0),
            Err(Error::Signature)
        );
        let mut tampered = ad.clone();
        tampered[32] = FLAG_UP;
        assert_eq!(
            verify_assertion(&cd, &tampered, &sig, CH, ORIGIN, RP, &r.key, 0),
            Err(Error::Flags)
        );
        let reg = Authenticator::client_json("webauthn.create", CH, ORIGIN);
        let (cd2, ad2, sig2) = a.get(RP, &reg);
        assert_eq!(
            verify_assertion(&cd2, &ad2, &sig2, CH, ORIGIN, RP, &r.key, 0),
            Err(Error::Type)
        );
    }

    #[test]
    fn cbor_is_bounded() {
        let mut deep = vec![0x81u8; 40];
        deep.push(0);
        assert!(cbor_parse(&deep).is_none());
        assert!(cbor_parse(&[0x5a, 0xff, 0xff, 0xff, 0xff]).is_none());
        assert!(cbor_parse(&[0x99, 0xff, 0xff]).is_none());
        assert_eq!(cbor_parse(&[0x18, 0x2a]), Some((Cbor::Uint(42), 2)));
        assert_eq!(cbor_parse(&[0x20]), Some((Cbor::Nint(-1), 1)));
    }
}
