#!/usr/bin/env python3
"""Minisign compatible keys and signatures for GodTerm's self update.

GodTerm verifies SHA256SUMS.minisig (made by `minisign -S` or by this
script) with the public key built into it (src/update.rs). The secret key
never enters the repository.

  minisign.py keygen SECRET_KEY_FILE PUBLIC_KEY_FILE
      A new unencrypted key pair (the secret file is 0600). The public key
      line goes into UPDATE_PUBKEY in src/update.rs.
  minisign.py sign SECRET_KEY_FILE FILE
      FILE.minisig: a prehashed (ED) signature, as `minisign -S -H` makes,
      verifiable with `minisign -Vm FILE -P <public key>`.

Needs Python 3 with the `cryptography` package.
"""
import base64
import hashlib
import os
import struct
import sys
import time

from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives import serialization


def raw_pk(sk):
    return sk.public_key().public_bytes(
        serialization.Encoding.Raw, serialization.PublicFormat.Raw
    )


def keygen(sec_path, pub_path):
    if os.path.exists(sec_path):
        sys.exit(f"{sec_path} exists; not overwriting a signing key")
    sk = Ed25519PrivateKey.generate()
    seed = sk.private_bytes(
        serialization.Encoding.Raw,
        serialization.PrivateFormat.Raw,
        serialization.NoEncryption(),
    )
    pk = raw_pk(sk)
    key_id = os.urandom(8)
    sk64 = seed + pk
    cksum = hashlib.blake2b(b"Ed" + key_id + sk64, digest_size=32).digest()
    blob = (
        b"Ed"
        + b"\x00\x00"  # kdf: none (unencrypted)
        + b"B2"
        + bytes(32)  # salt
        + struct.pack("<Q", 0)
        + struct.pack("<Q", 0)
        + key_id
        + sk64
        + cksum
    )
    kid = key_id[::-1].hex().upper()
    d = os.path.dirname(os.path.abspath(sec_path))
    os.makedirs(d, mode=0o700, exist_ok=True)
    fd = os.open(sec_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(f"untrusted comment: minisign encrypted secret key\n")
        f.write(base64.b64encode(blob).decode() + "\n")
    pub = base64.b64encode(b"Ed" + key_id + pk).decode()
    with open(pub_path, "w") as f:
        f.write(f"untrusted comment: minisign public key {kid}\n{pub}\n")
    print(pub)


def load_secret(path):
    lines = open(path).read().splitlines()
    blob = base64.b64decode(lines[1])
    if blob[:2] != b"Ed" or blob[2:4] != b"\x00\x00":
        sys.exit("only unencrypted Ed25519 minisign keys are supported here")
    off = 2 + 2 + 2 + 32 + 8 + 8
    key_id = blob[off : off + 8]
    sk64 = blob[off + 8 : off + 72]
    cksum = blob[off + 72 : off + 104]
    if hashlib.blake2b(b"Ed" + key_id + sk64, digest_size=32).digest() != cksum:
        sys.exit("secret key checksum mismatch")
    return key_id, Ed25519PrivateKey.from_private_bytes(sk64[:32])


def sign(sec_path, file_path):
    key_id, sk = load_secret(sec_path)
    data = open(file_path, "rb").read()
    sig = sk.sign(hashlib.blake2b(data).digest())
    trusted = f"timestamp:{int(time.time())}\tfile:{os.path.basename(file_path)}\thashed"
    global_sig = sk.sign(sig + trusted.encode())
    out = file_path + ".minisig"
    with open(out, "w") as f:
        f.write("untrusted comment: signature from minisign secret key\n")
        f.write(base64.b64encode(b"ED" + key_id + sig).decode() + "\n")
        f.write(f"trusted comment: {trusted}\n")
        f.write(base64.b64encode(global_sig).decode() + "\n")
    print(out)


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "keygen":
        keygen(sys.argv[2], sys.argv[3])
    elif len(sys.argv) == 4 and sys.argv[1] == "sign":
        sign(sys.argv[2], sys.argv[3])
    else:
        sys.exit(__doc__)
