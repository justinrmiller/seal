"""
Deterministic bot system: 100 bots, 10 channels, up to 5 messages each.

Usage:
    uv run python scripts/bots.py [--base-url http://localhost:8000]

Bots are deterministically seeded so results are reproducible.
Uses real X25519 + crypto_box encryption (via PyNaCl) so messages
are decryptable by real clients.
"""

import argparse
import base64
import json
import random
import time

import nacl.public
import nacl.utils
import requests

SEED = 42          # Used only for deterministic key generation (account creation)
NUM_BOTS = 100
NUM_CHANNELS = 10
MAX_MESSAGES_PER_BOT = 5

MESSAGES = [
    "Hello everyone!",
    "How's it going?",
    "Just checking in.",
    "Anyone here?",
    "Great to be here!",
    "What's new?",
    "Good morning!",
    "Interesting discussion.",
    "I agree with that.",
    "Let me think about it.",
    "That's a great point.",
    "Thanks for sharing!",
    "Can someone help me?",
    "I'm new here.",
    "Happy to join this channel!",
    "See you all later.",
    "Working on something cool.",
    "Has anyone tried this?",
    "Let's collaborate!",
    "Just finished a task.",
    "Any updates?",
    "Looking forward to it.",
    "Count me in!",
    "That makes sense.",
    "I'll follow up on that.",
    "Sounds good to me.",
    "Let me know if you need help.",
    "On it!",
    "Noted.",
    "This is awesome!",
]


def b64enc(data: bytes) -> str:
    return base64.b64encode(data).decode()


def b64dec(s: str) -> bytes:
    return base64.b64decode(s)


def bot_name(i: int) -> str:
    return f"bot{i:03d}"


def bot_password(i: int) -> str:
    return f"botpass{i:03d}"


def channel_name(i: int) -> str:
    return f"channel-{i:02d}"


def generate_keypair(rng: random.Random) -> tuple[nacl.public.PrivateKey, nacl.public.PublicKey]:
    """Generate a deterministic X25519 key pair from the seeded RNG."""
    seed_bytes = bytes(rng.getrandbits(8) for _ in range(32))
    private_key = nacl.public.PrivateKey(seed_bytes)
    return private_key, private_key.public_key


def encrypt_for(plaintext: str, recipient_pub_bytes: bytes) -> dict:
    """Encrypt plaintext for a recipient using an ephemeral key pair.

    Matches the JS CRYPTO.encrypt() format:
    - Generate ephemeral X25519 key pair
    - Use crypto_box (XSalsa20-Poly1305) with ephemeral private + recipient public
    - Return {ciphertext, iv, sender_public_key_jwk} all base64-encoded
    """
    ephemeral_private = nacl.public.PrivateKey.generate()
    ephemeral_public = ephemeral_private.public_key
    recipient_pub = nacl.public.PublicKey(recipient_pub_bytes)
    box = nacl.public.Box(ephemeral_private, recipient_pub)
    nonce = nacl.utils.random(nacl.public.Box.NONCE_SIZE)
    encrypted = box.encrypt(plaintext.encode(), nonce)

    return {
        "ciphertext": b64enc(encrypted.ciphertext),
        "iv": b64enc(nonce),
        "sender_public_key_jwk": b64enc(ephemeral_public.encode()),
    }


def register_or_login(session: requests.Session, base_url: str,
                       username: str, password: str, pub_key_b64: str) -> dict:
    r = session.post(f"{base_url}/api/register", json={
        "username": username,
        "password": password,
        "public_key_jwk": pub_key_b64,
    })
    if r.status_code == 400 and "already taken" in r.text.lower():
        r = session.post(f"{base_url}/api/login", json={
            "username": username,
            "password": password,
        })
    r.raise_for_status()
    return r.json()


def create_channel(base_url: str, token: str, name: str, members: list[str]) -> dict | None:
    r = requests.post(
        f"{base_url}/api/channels",
        params={"token": token},
        json={"name": name, "members": members},
    )
    if r.status_code == 400:
        return None
    r.raise_for_status()
    return r.json()


def send_message(session: requests.Session, base_url: str,
                 token: str, channel_id: str, text: str):
    """Fetch member public keys, encrypt for each, send via REST."""
    r = session.get(
        f"{base_url}/api/channels/{channel_id}/members/public_keys",
        params={"token": token},
    )
    r.raise_for_status()

    envelopes = []
    for m in r.json():
        recipient_pub_bytes = b64dec(m["public_key_jwk"])
        encrypted = encrypt_for(text, recipient_pub_bytes)
        envelopes.append({"target_user": m["username"], **encrypted})

    r = session.post(
        f"{base_url}/api/channels/{channel_id}/messages",
        params={"token": token},
        json={"channel_id": channel_id, "envelopes": envelopes},
    )
    r.raise_for_status()


def main():
    parser = argparse.ArgumentParser(description="Run 100 bots sending messages to 10 channels")
    parser.add_argument("--base-url", default="http://localhost:8000", help="Server base URL")
    args = parser.parse_args()
    base_url = args.base_url

    key_rng = random.Random(SEED)   # Deterministic for reproducible keys
    msg_rng = random.Random()       # Random for fresh messages each run
    session = requests.Session()

    # Step 1: Generate key pairs and register all bots
    print(f"Registering {NUM_BOTS} bots...")
    tokens = {}
    keys = {}

    for i in range(NUM_BOTS):
        name = bot_name(i)
        priv, pub = generate_keypair(key_rng)
        keys[name] = (priv, pub)
        pub_b64 = b64enc(pub.encode())
        result = register_or_login(session, base_url, name, bot_password(i), pub_b64)
        tokens[name] = result["token"]
        print(f"  [{i+1:3d}/{NUM_BOTS}] {name}", end="\r")
    print(f"  Registered {NUM_BOTS} bots.           ")

    # Step 2: Create 10 channels (bot000 creates them, all bots as members)
    print(f"Creating {NUM_CHANNELS} channels...")
    creator = bot_name(0)
    creator_token = tokens[creator]
    all_bots = [bot_name(i) for i in range(NUM_BOTS)]
    other_bots = [b for b in all_bots if b != creator]
    channel_ids = {}

    for i in range(NUM_CHANNELS):
        name = channel_name(i)
        result = create_channel(base_url, creator_token, name, other_bots)
        if result:
            channel_ids[name] = result["id"]
            print(f"  Created #{name}")
        else:
            r = session.get(f"{base_url}/api/channels", params={"token": creator_token})
            r.raise_for_status()
            for ch in r.json():
                if ch["name"] == name:
                    channel_ids[name] = ch["id"]
                    print(f"  Found #{name}")
                    break

    channel_id_list = list(channel_ids.values())

    # Step 3: Build randomized message schedule
    schedule = []
    for i in range(NUM_BOTS):
        name = bot_name(i)
        num_msgs = msg_rng.randint(1, MAX_MESSAGES_PER_BOT)
        for _ in range(num_msgs):
            schedule.append((name, msg_rng.choice(channel_id_list), msg_rng.choice(MESSAGES)))

    msg_rng.shuffle(schedule)
    total = len(schedule)
    print(f"Sending {total} messages...")

    # Step 4: Send messages
    sent = 0
    t0 = time.time()
    for idx, (sender, ch_id, text) in enumerate(schedule):
        try:
            send_message(session, base_url, tokens[sender], ch_id, text)
            sent += 1
        except Exception as e:
            print(f"\n  Error ({sender}): {e}")
        print(f"  [{idx+1:3d}/{total}] {sender}: {text[:40]}", end="\r")

    elapsed = time.time() - t0
    print(f"\nDone! Sent {sent}/{total} messages in {elapsed:.1f}s "
          f"({sent/elapsed:.1f} msg/s) from {NUM_BOTS} bots across {NUM_CHANNELS} channels.")


if __name__ == "__main__":
    main()
