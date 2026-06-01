/**
 * E2E Encryption module using libsodium (trusted, audited library).
 *
 * Primitives:
 *   - X25519 for key exchange (ECDH)
 *   - XSalsa20-Poly1305 for authenticated encryption (crypto_box)
 *   - Argon2id + XSalsa20-Poly1305 for password-protected key export
 *
 * Flow:
 *   1. Each user generates an X25519 key pair on registration.
 *   2. To send a message, generate an ephemeral X25519 key pair,
 *      compute a shared secret with the recipient's public key,
 *      and encrypt with crypto_box.
 *   3. The ephemeral public key is sent alongside the ciphertext.
 *
 * Each message uses a new ephemeral key -> forward secrecy per message.
 */

let sodiumReady = false;

async function initCrypto() {
    await sodium.ready;
    sodiumReady = true;
}

const CRYPTO = {
    async ensureReady() {
        if (!sodiumReady) await initCrypto();
    },

    /** Generate a new X25519 key pair */
    async generateKeyPair() {
        await this.ensureReady();
        const kp = sodium.crypto_box_keypair();
        return { publicKey: kp.publicKey, privateKey: kp.privateKey };
    },

    /** Export public key to base64 string */
    exportPublicKey(publicKey) {
        return sodium.to_base64(publicKey, sodium.base64_variants.ORIGINAL);
    },

    /** Import public key from base64 string */
    importPublicKey(b64) {
        return sodium.from_base64(b64, sodium.base64_variants.ORIGINAL);
    },

    /** Export private key to base64 string */
    exportPrivateKey(privateKey) {
        return sodium.to_base64(privateKey, sodium.base64_variants.ORIGINAL);
    },

    /** Import private key from base64 string */
    importPrivateKey(b64) {
        return sodium.from_base64(b64, sodium.base64_variants.ORIGINAL);
    },

    /**
     * Encrypt plaintext for a recipient. Uses ephemeral key pair for forward secrecy.
     * Returns { ciphertext, iv, ephemeralPublicKey } all base64-encoded.
     */
    async encrypt(plaintext, recipientPublicKey) {
        await this.ensureReady();
        const ephemeral = sodium.crypto_box_keypair();
        const nonce = sodium.randombytes_buf(sodium.crypto_box_NONCEBYTES);
        const message = sodium.from_string(plaintext);
        const ciphertext = sodium.crypto_box_easy(
            message, nonce, recipientPublicKey, ephemeral.privateKey
        );
        return {
            ciphertext: sodium.to_base64(ciphertext, sodium.base64_variants.ORIGINAL),
            iv: sodium.to_base64(nonce, sodium.base64_variants.ORIGINAL),
            ephemeralPublicKey: sodium.to_base64(ephemeral.publicKey, sodium.base64_variants.ORIGINAL),
        };
    },

    /**
     * Decrypt ciphertext using our private key and the sender's ephemeral public key.
     */
    async decrypt(ciphertextB64, nonceB64, senderEphemeralPublicKeyB64, myPrivateKey) {
        await this.ensureReady();
        const ciphertext = sodium.from_base64(ciphertextB64, sodium.base64_variants.ORIGINAL);
        const nonce = sodium.from_base64(nonceB64, sodium.base64_variants.ORIGINAL);
        const senderPub = sodium.from_base64(senderEphemeralPublicKeyB64, sodium.base64_variants.ORIGINAL);
        const plaintext = sodium.crypto_box_open_easy(ciphertext, nonce, senderPub, myPrivateKey);
        return sodium.to_string(plaintext);
    },

    // --- Symmetric encryption for channel attachments (secretbox) ---

    /**
     * Encrypt data with a random symmetric key (for channel image attachments).
     * Returns { key: Uint8Array, ciphertext: base64, iv: base64 }
     */
    async encryptSymmetric(plainBytes) {
        await this.ensureReady();
        const key = sodium.crypto_secretbox_keygen();
        const nonce = sodium.randombytes_buf(sodium.crypto_secretbox_NONCEBYTES);
        const ciphertext = sodium.crypto_secretbox_easy(plainBytes, nonce, key);
        return {
            key,
            ciphertext: sodium.to_base64(ciphertext, sodium.base64_variants.ORIGINAL),
            iv: sodium.to_base64(nonce, sodium.base64_variants.ORIGINAL),
        };
    },

    /**
     * Decrypt data encrypted with encryptSymmetric.
     */
    async decryptSymmetric(ciphertextB64, ivB64, key) {
        await this.ensureReady();
        const ciphertext = sodium.from_base64(ciphertextB64, sodium.base64_variants.ORIGINAL);
        const nonce = sodium.from_base64(ivB64, sodium.base64_variants.ORIGINAL);
        return sodium.crypto_secretbox_open_easy(ciphertext, nonce, key);
    },

    // --- Password-protected key export/import (Argon2id + secretbox) ---

    /**
     * Encrypt a private key with a password for secure export.
     * Uses Argon2id (memory-hard) to derive a symmetric key, then secretbox.
     * Returns { salt, nonce, data } all base64-encoded.
     */
    async encryptPrivateKeyWithPassword(privateKey, password) {
        await this.ensureReady();
        const salt = sodium.randombytes_buf(sodium.crypto_pwhash_SALTBYTES);
        const key = sodium.crypto_pwhash(
            sodium.crypto_secretbox_KEYBYTES,
            password,
            salt,
            sodium.crypto_pwhash_OPSLIMIT_INTERACTIVE,
            sodium.crypto_pwhash_MEMLIMIT_INTERACTIVE,
            sodium.crypto_pwhash_ALG_ARGON2ID13
        );
        const nonce = sodium.randombytes_buf(sodium.crypto_secretbox_NONCEBYTES);
        const encrypted = sodium.crypto_secretbox_easy(privateKey, nonce, key);
        return {
            salt: sodium.to_base64(salt, sodium.base64_variants.ORIGINAL),
            nonce: sodium.to_base64(nonce, sodium.base64_variants.ORIGINAL),
            data: sodium.to_base64(encrypted, sodium.base64_variants.ORIGINAL),
        };
    },

    /**
     * Decrypt a private key from a password-protected export.
     * Returns Uint8Array private key.
     */
    async decryptPrivateKeyWithPassword(encryptedBundle, password) {
        await this.ensureReady();
        const salt = sodium.from_base64(encryptedBundle.salt, sodium.base64_variants.ORIGINAL);
        const nonce = sodium.from_base64(encryptedBundle.nonce, sodium.base64_variants.ORIGINAL);
        const data = sodium.from_base64(encryptedBundle.data, sodium.base64_variants.ORIGINAL);
        const key = sodium.crypto_pwhash(
            sodium.crypto_secretbox_KEYBYTES,
            password,
            salt,
            sodium.crypto_pwhash_OPSLIMIT_INTERACTIVE,
            sodium.crypto_pwhash_MEMLIMIT_INTERACTIVE,
            sodium.crypto_pwhash_ALG_ARGON2ID13
        );
        return sodium.crypto_secretbox_open_easy(data, nonce, key);
    },
};

// --- IndexedDB key storage ---

const DB_NAME = "e2e-chat-keys";
const DB_VERSION = 1;
const STORE_NAME = "keys";

function openKeyDB() {
    return new Promise((resolve, reject) => {
        const req = indexedDB.open(DB_NAME, DB_VERSION);
        req.onupgradeneeded = () => {
            req.result.createObjectStore(STORE_NAME, { keyPath: "username" });
        };
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
    });
}

async function storeKeysIDB(username, privateKeyB64, publicKeyB64) {
    const db = await openKeyDB();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, "readwrite");
        tx.objectStore(STORE_NAME).put({ username, privateKey: privateKeyB64, publicKey: publicKeyB64 });
        tx.oncomplete = () => resolve();
        tx.onerror = () => reject(tx.error);
    });
}

async function loadKeysIDB(username) {
    const db = await openKeyDB();
    return new Promise((resolve, reject) => {
        const tx = db.transaction(STORE_NAME, "readonly");
        const req = tx.objectStore(STORE_NAME).get(username);
        req.onsuccess = () => resolve(req.result || null);
        req.onerror = () => reject(req.error);
    });
}
