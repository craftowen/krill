#!/usr/bin/env python3
"""Pure-stdlib RFC 8032 Ed25519 keygen and latest.json manifest signer for Krill (MK-154).

Uses only Python 3 standard library modules (hashlib, os, json, argparse, stat, pathlib).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import stat
import sys
from pathlib import Path

# Ed25519 curve constants (RFC 8032 §5.1)
P = (1 << 255) - 19
L = (1 << 252) + 27742317777372353535851937790883648493
D = (-121665 * pow(121666, P - 2, P)) % P
I_SQRT = pow(2, (P - 1) // 4, P)

B_Y = (4 * pow(5, P - 2, P)) % P
B_X = 15112221349535400772501151409588531511454012693041857206046113283949847762202
B_POINT = (B_X, B_Y, 1, (B_X * B_Y) % P)
NEUTRAL = (0, 1, 1, 0)

# RFC 8032 §7.1 test vectors are forbidden for production/release signing (updater.rs TEST_PUBLIC_KEYS)
FORBIDDEN_TEST_SEEDS = frozenset(
    {
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
    }
)
FORBIDDEN_TEST_PUBKEYS = frozenset(
    {
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
    }
)

Point = tuple[int, int, int, int]


def point_add(p: Point, q: Point) -> Point:
    x1, y1, z1, t1 = p
    x2, y2, z2, t2 = q
    a = ((y1 - x1) * (y2 - x2)) % P
    b = ((y1 + x1) * (y2 + x2)) % P
    c = (t1 * 2 * D * t2) % P
    d = (z1 * 2 * z2) % P
    e = (b - a) % P
    f = (d - c) % P
    g = (d + c) % P
    h = (b + a) % P
    return ((e * f) % P, (g * h) % P, (f * g) % P, (e * h) % P)


def point_double(p: Point) -> Point:
    x1, y1, z1, _ = p
    a = (x1 * x1) % P
    b = (y1 * y1) % P
    c = (2 * z1 * z1) % P
    h = (a + b) % P
    x_plus_y = (x1 + y1) % P
    e = (h - (x_plus_y * x_plus_y) % P) % P
    g = (a - b) % P
    f = (c + g) % P
    return ((e * f) % P, (g * h) % P, (f * g) % P, (e * h) % P)


def scalar_mult(k: int, p: Point) -> Point:
    result = NEUTRAL
    addend = p
    while k > 0:
        if k & 1:
            result = point_add(result, addend)
        addend = point_double(addend)
        k >>= 1
    return result


def point_encode(p: Point) -> bytes:
    x, y, z, _ = p
    z_inv = pow(z, P - 2, P)
    x_aff = (x * z_inv) % P
    y_aff = (y * z_inv) % P
    encoded = y_aff | ((x_aff & 1) << 255)
    return encoded.to_bytes(32, "little")


def point_decode(encoded: bytes) -> Point:
    if len(encoded) != 32:
        raise ValueError("Ed25519 encoded point must be 32 bytes")
    unpacked = int.from_bytes(encoded, "little")
    x_sign = (unpacked >> 255) & 1
    y = unpacked & ((1 << 255) - 1)
    if y >= P:
        raise ValueError("Ed25519 y coordinate out of range")
    y2 = (y * y) % P
    u = (y2 - 1) % P
    v = (D * y2 + 1) % P
    v_inv = pow(v, P - 2, P)
    u_div_v = (u * v_inv) % P
    x = pow(u_div_v, (P + 3) // 8, P)
    if (v * x * x - u) % P != 0:
        x = (x * I_SQRT) % P
    if (v * x * x - u) % P != 0:
        raise ValueError("Ed25519 point is not on curve")
    if x == 0 and x_sign == 1:
        raise ValueError("Ed25519 invalid x=0 with sign bit 1")
    if (x & 1) != x_sign:
        x = P - x
    return (x, y, 1, (x * y) % P)


def _expand_seed(seed: bytes) -> tuple[int, bytes, bytes]:
    if len(seed) != 32:
        raise ValueError("Ed25519 seed must be 32 bytes")
    h = bytearray(hashlib.sha512(seed).digest())
    h[0] &= 248
    h[31] &= 127
    h[31] |= 64
    s = int.from_bytes(h[:32], "little")
    prefix = bytes(h[32:64])
    pub = point_encode(scalar_mult(s, B_POINT))
    return s, prefix, pub


def public_key_from_seed(seed: bytes) -> bytes:
    _, _, pub = _expand_seed(seed)
    return pub


def ed25519_sign(seed: bytes, message: bytes) -> bytes:
    s, prefix, pub = _expand_seed(seed)
    r = int.from_bytes(hashlib.sha512(prefix + message).digest(), "little") % L
    r_bytes = point_encode(scalar_mult(r, B_POINT))
    k = int.from_bytes(hashlib.sha512(r_bytes + pub + message).digest(), "little") % L
    s_scalar = (r + k * s) % L
    return r_bytes + s_scalar.to_bytes(32, "little")


def ed25519_verify(pub: bytes, message: bytes, sig: bytes) -> bool:
    if len(pub) != 32 or len(sig) != 64:
        return False
    r_bytes = sig[:32]
    s_scalar = int.from_bytes(sig[32:], "little")
    if s_scalar >= L:
        return False
    try:
        a_point = point_decode(pub)
        r_point = point_decode(r_bytes)
    except ValueError:
        return False
    k = int.from_bytes(hashlib.sha512(r_bytes + pub + message).digest(), "little") % L
    lhs = scalar_mult(s_scalar, B_POINT)
    rhs = point_add(r_point, scalar_mult(k, a_point))
    return point_encode(lhs) == point_encode(rhs)


def parse_and_validate_seed_hex(seed_hex: str) -> bytes:
    clean = seed_hex.strip().lower()
    if len(clean) != 64:
        raise ValueError("Ed25519 seed must be a 64-character hex string (32 bytes)")
    try:
        seed = bytes.fromhex(clean)
    except ValueError as exc:
        raise ValueError("Ed25519 seed contains non-hex characters") from exc
    if clean in FORBIDDEN_TEST_SEEDS:
        raise ValueError("RFC 8032 test secret seed is forbidden for release signing")
    pub_hex = public_key_from_seed(seed).hex()
    if pub_hex in FORBIDDEN_TEST_PUBKEYS:
        raise ValueError("RFC 8032 test public key is forbidden for release signing")
    return seed


def load_or_create_seed(key_file: Path, create_if_missing: bool) -> bytes:
    env_seed = os.environ.get("KRILL_RELEASE_SIGNING_KEY", "").strip()
    if env_seed:
        return parse_and_validate_seed_hex(env_seed)

    if key_file.is_file():
        return parse_and_validate_seed_hex(key_file.read_text(encoding="utf-8"))

    if not create_if_missing:
        raise FileNotFoundError(
            f"Ed25519 signing key not found at {key_file} "
            "(run keygen or set KRILL_RELEASE_SIGNING_KEY)"
        )

    key_file.parent.mkdir(parents=True, exist_ok=True)
    while True:
        seed = os.urandom(32)
        seed_hex = seed.hex()
        pub_hex = public_key_from_seed(seed).hex()
        if seed_hex not in FORBIDDEN_TEST_SEEDS and pub_hex not in FORBIDDEN_TEST_PUBKEYS:
            break

    fd = os.open(
        str(key_file),
        os.O_WRONLY | os.O_CREAT | os.O_TRUNC,
        stat.S_IRUSR | stat.S_IWUSR,
    )
    try:
        os.write(fd, (seed_hex + "\n").encode("utf-8"))
    finally:
        os.close(fd)
    os.chmod(key_file, stat.S_IRUSR | stat.S_IWUSR)
    return seed


def canonical_signing_payload(
    version: str,
    url: str,
    sha256: str,
    min_daemon_protocol: int,
    channel: str,
    rollout_pct: int,
) -> bytes:
    return (
        "minikrill-update-v1\n"
        f"version={version}\n"
        f"url={url}\n"
        f"sha256={sha256.lower()}\n"
        f"min_daemon_protocol={min_daemon_protocol}\n"
        f"channel={channel}\n"
        f"rollout_pct={rollout_pct}\n"
    ).encode("utf-8")


def cmd_keygen(args: argparse.Namespace) -> int:
    key_path = Path(args.key_file)
    seed = load_or_create_seed(key_path, create_if_missing=True)
    pub_hex = public_key_from_seed(seed).hex()
    if not os.environ.get("KRILL_RELEASE_SIGNING_KEY", "").strip():
        pub_path = Path(f"{key_path}.pub")
        pub_path.parent.mkdir(parents=True, exist_ok=True)
        pub_path.write_text(pub_hex + "\n", encoding="utf-8")
    sys.stdout.write(pub_hex + "\n")
    return 0


def cmd_sign(args: argparse.Namespace) -> int:
    seed = load_or_create_seed(Path(args.key_file), create_if_missing=False)
    sha256_clean = args.sha256.strip().lower()
    if len(sha256_clean) != 64:
        raise ValueError("sha256 must be a 64-character hex string")
    bytes.fromhex(sha256_clean)

    if not (0 <= args.rollout_pct <= 100):
        raise ValueError("rollout_pct must be between 0 and 100")
    if not (0 <= args.min_daemon_protocol <= 65535):
        raise ValueError("min_daemon_protocol must fit in u16 (0..=65535)")

    payload = canonical_signing_payload(
        version=args.version,
        url=args.url,
        sha256=sha256_clean,
        min_daemon_protocol=args.min_daemon_protocol,
        channel=args.channel,
        rollout_pct=args.rollout_pct,
    )
    pub = public_key_from_seed(seed)
    sig = ed25519_sign(seed, payload)
    if not ed25519_verify(pub, payload, sig):
        raise RuntimeError("internal Ed25519 self-verification failed")

    manifest = {
        "version": args.version,
        "url": args.url,
        "sha256": sha256_clean,
        "ed25519_sig": sig.hex(),
        "min_daemon_protocol": args.min_daemon_protocol,
        "channel": args.channel,
        "rollout_pct": args.rollout_pct,
    }
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Ed25519 release signing key and latest.json manifest generator"
    )
    sub = parser.add_subparsers(dest="subcommand", required=True)

    p_keygen = sub.add_parser("keygen", help="Ensure Ed25519 key exists and print 64-hex public key")
    p_keygen.add_argument("--key-file", required=True, help="Path to 64-hex Ed25519 secret seed file")
    p_keygen.set_defaults(func=cmd_keygen)

    p_sign = sub.add_parser("sign", help="Sign and write latest.json manifest")
    p_sign.add_argument("--key-file", required=True, help="Path to 64-hex Ed25519 secret seed file")
    p_sign.add_argument("--version", required=True, help="Release version (e.g. 0.1.1)")
    p_sign.add_argument("--url", required=True, help="Download URL for Krill-<ver>-macos-arm64.zip")
    p_sign.add_argument("--sha256", required=True, help="64-hex SHA-256 of the zip archive")
    p_sign.add_argument(
        "--min-daemon-protocol",
        type=int,
        default=1,
        help="Minimum compatible daemon protocol version (u16)",
    )
    p_sign.add_argument(
        "--channel",
        choices=("stable", "beta"),
        default="stable",
        help="Release channel (stable or beta)",
    )
    p_sign.add_argument(
        "--rollout-pct",
        type=int,
        default=100,
        help="Rollout percentage (0..=100)",
    )
    p_sign.add_argument("--output", required=True, help="Output path for latest.json")
    p_sign.set_defaults(func=cmd_sign)

    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except Exception as exc:  # noqa: BLE001
        sys.stderr.write(f"ed25519_manifest.py: error: {exc}\n")
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
