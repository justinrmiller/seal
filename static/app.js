/**
 * Chat application — DMs and E2E encrypted channels.
 * All encryption/decryption happens in the browser; the server only sees opaque ciphertext.
 * Keys stored in IndexedDB (not raw localStorage strings).
 */

let token = null;
let username = null;
let privateKey = null;   // Uint8Array
let publicKeyB64 = null; // base64 string
let ws = null;
let refreshInterval = null;

// Current active conversation: { type: "dm"|"channel", id: <username|channelId> }
let activeConvo = null;

let peerPublicKeys = {};   // username -> Uint8Array public key
let messageCache = {};     // convoKey -> [{id, sender, text, timestamp}]
let channelsList = [];     // [{id, name, created_by, members}]
let allUsers = [];         // [{username}]
let pendingImage = null;   // { data: Uint8Array, mime: string, name: string }

const MAX_IMAGE_SIZE = 5 * 1024 * 1024; // 5MB

// --- DOM refs ---
const authScreen = document.getElementById("auth-screen");
const chatScreen = document.getElementById("chat-screen");
const authError = document.getElementById("auth-error");
const usernameInput = document.getElementById("auth-username");
const passwordInput = document.getElementById("auth-password");
const userList = document.getElementById("user-list");
const channelListDiv = document.getElementById("channel-list");
const messagesDiv = document.getElementById("messages");
const chatHeader = document.getElementById("chat-header-name");
const chatHeaderInfo = document.getElementById("chat-header-info");
const chatWith = document.getElementById("chat-with");
const noChat = document.getElementById("no-chat");
const msgInput = document.getElementById("msg-input");
const sendBtn = document.getElementById("send-btn");
const currentUser = document.getElementById("current-user");
const modalOverlay = document.getElementById("modal-overlay");
const channelNameInput = document.getElementById("channel-name-input");
const memberCheckboxes = document.getElementById("member-checkboxes");
const modalError = document.getElementById("modal-error");
const keyPasswordModal = document.getElementById("key-password-modal");
const keyPasswordInput = document.getElementById("key-password-input");
const keyPasswordError = document.getElementById("key-password-error");
const keyPasswordTitle = document.getElementById("key-password-title");
const inviteBtn = document.getElementById("btn-invite");

function convoKey(type, id) { return `${type}:${id}`; }

function showAuthError(msg, isSuccess) {
    authError.style.color = isSuccess ? "#4caf50" : "";
    authError.textContent = msg;
    if (isSuccess) setTimeout(() => { authError.style.color = ""; }, 5000);
}

// --- Password prompt modal ---

let keyPasswordResolve = null;

function promptPassword(title) {
    return new Promise((resolve) => {
        keyPasswordTitle.textContent = title;
        keyPasswordInput.value = "";
        keyPasswordError.textContent = "";
        keyPasswordModal.style.display = "flex";
        keyPasswordInput.focus();
        keyPasswordResolve = resolve;
    });
}

function closeKeyPasswordModal(result) {
    keyPasswordModal.style.display = "none";
    if (keyPasswordResolve) {
        keyPasswordResolve(result);
        keyPasswordResolve = null;
    }
}

// --- Key export/import (password-protected with Argon2id) ---

async function exportKeysToFile(user) {
    const stored = await loadKeysIDB(user);
    if (!stored) {
        showAuthError("No keys found for this username on this device.", false);
        return;
    }

    const password = await promptPassword("Enter a password to encrypt your key backup:");
    if (!password) return;

    try {
        await CRYPTO.ensureReady();
        const privKeyBytes = CRYPTO.importPrivateKey(stored.privateKey);
        const encryptedPrivate = await CRYPTO.encryptPrivateKeyWithPassword(privKeyBytes, password);

        const payload = JSON.stringify({
            version: 2,
            username: user,
            publicKey: stored.publicKey,
            encryptedPrivateKey: encryptedPrivate,
            exportedAt: new Date().toISOString(),
            warning: "Private key is password-encrypted (Argon2id). Keep this file safe.",
        }, null, 2);

        const filename = `e2e-keys-${user}.json`;
        const blob = new Blob([payload], { type: "application/json" });
        const url = URL.createObjectURL(blob);
        const a = document.createElement("a");
        a.href = url;
        a.download = filename;
        a.click();
        URL.revokeObjectURL(url);
        showAuthError(`Keys exported to your Downloads folder as "${filename}"`, true);
    } catch (e) {
        showAuthError("Failed to export keys: " + e.message, false);
    }
}

async function importKeysFromFile(file) {
    const text = await file.text();
    let data;
    try {
        data = JSON.parse(text);
    } catch (e) {
        showAuthError("Failed to parse key file.", false);
        return;
    }

    await CRYPTO.ensureReady();

    if (data.version === 2 && data.encryptedPrivateKey) {
        // Password-protected format
        const password = await promptPassword("Enter the password used to encrypt this key backup:");
        if (!password) return;

        try {
            const privKeyBytes = await CRYPTO.decryptPrivateKeyWithPassword(data.encryptedPrivateKey, password);
            const privKeyB64 = CRYPTO.exportPrivateKey(privKeyBytes);
            await storeKeysIDB(data.username, privKeyB64, data.publicKey);
            usernameInput.value = data.username;
            showAuthError(`Keys imported for "${data.username}". Enter your password and sign in.`, true);
        } catch (e) {
            showAuthError("Wrong password or corrupted key file.", false);
        }
    } else if (data.privateKey && data.publicKey) {
        // Legacy unencrypted format
        await storeKeysIDB(data.username, data.privateKey, data.publicKey);
        usernameInput.value = data.username;
        showAuthError(`Keys imported for "${data.username}". Sign in to continue.`, true);
    } else {
        showAuthError("Unrecognized key file format.", false);
    }
}

// --- Auth ---

async function register() {
    const user = usernameInput.value.trim();
    const pass = passwordInput.value.trim();
    if (!user || !pass) {
        showAuthError("Username and password are required", false);
        return;
    }

    try {
        await CRYPTO.ensureReady();
        const kp = await CRYPTO.generateKeyPair();
        const pubB64 = CRYPTO.exportPublicKey(kp.publicKey);
        const privB64 = CRYPTO.exportPrivateKey(kp.privateKey);

        const res = await fetch("/api/register", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ username: user, password: pass, public_key_jwk: pubB64 }),
        });

        if (!res.ok) {
            let msg = "Registration failed (status " + res.status + ")";
            try { const err = await res.json(); msg = err.detail || msg; } catch {}
            showAuthError(msg, false);
            return;
        }

        await storeKeysIDB(user, privB64, pubB64);
        const data = await res.json();
        enterChat(data.token, user, kp.privateKey, pubB64);
    } catch (err) {
        console.error("Registration error:", err);
        showAuthError("Registration failed: " + err.message, false);
    }
}

async function login() {
    const user = usernameInput.value.trim();
    const pass = passwordInput.value.trim();
    if (!user || !pass) return;

    const res = await fetch("/api/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username: user, password: pass }),
    });

    if (!res.ok) {
        let msg = "Login failed";
        try { const err = await res.json(); msg = err.detail || msg; } catch {}
        showAuthError(msg, false);
        return;
    }

    const stored = await loadKeysIDB(user);
    if (!stored) {
        showAuthError("No encryption keys found on this device. Import your keys first.", false);
        return;
    }

    await CRYPTO.ensureReady();
    const privKey = CRYPTO.importPrivateKey(stored.privateKey);
    const data = await res.json();
    enterChat(data.token, user, privKey, stored.publicKey);
}

// --- Logout ---

function logout() {
    if (ws) { ws.close(); ws = null; }
    if (refreshInterval) { clearInterval(refreshInterval); refreshInterval = null; }

    token = null;
    username = null;
    privateKey = null;
    publicKeyB64 = null;
    activeConvo = null;
    peerPublicKeys = {};
    messageCache = {};
    channelsList = [];
    allUsers = [];

    chatScreen.style.display = "none";
    authScreen.style.display = "flex";
    passwordInput.value = "";
    authError.textContent = "";
    messagesDiv.innerHTML = "";
    userList.innerHTML = "";
    channelListDiv.innerHTML = "";
    chatWith.style.display = "none";
    noChat.style.display = "flex";
}

// --- Chat setup ---

function enterChat(tok, user, privKey, pubB64) {
    token = tok;
    username = user;
    privateKey = privKey;
    publicKeyB64 = pubB64;

    authScreen.style.display = "none";
    chatScreen.style.display = "block";
    currentUser.textContent = user;

    loadSidebar();
    connectWs();
}

async function loadSidebar() {
    const usersRes = await fetch(`/api/users?token=${token}`);
    allUsers = await usersRes.json();
    renderUserList();

    const chRes = await fetch(`/api/channels?token=${token}`);
    channelsList = await chRes.json();
    renderChannelList();
}

function renderUserList() {
    userList.innerHTML = "";
    for (const u of allUsers) {
        const div = document.createElement("div");
        div.className = "sidebar-item";
        div.textContent = u.username;
        div.dataset.type = "dm";
        div.dataset.id = u.username;
        div.onclick = () => selectConvo("dm", u.username, div);
        userList.appendChild(div);
    }
}

function renderChannelList() {
    channelListDiv.innerHTML = "";
    for (const ch of channelsList) {
        const div = document.createElement("div");
        div.className = "sidebar-item";
        div.textContent = "# " + ch.name;
        div.dataset.type = "channel";
        div.dataset.id = ch.id;
        div.onclick = () => selectConvo("channel", ch.id, div);
        if (activeConvo && activeConvo.type === "channel" && activeConvo.id === ch.id) {
            div.classList.add("active");
        }
        channelListDiv.appendChild(div);
    }
}

// --- Conversation selection ---

async function selectConvo(type, id, el) {
    activeConvo = { type, id };
    document.querySelectorAll(".sidebar-item").forEach(e => e.classList.remove("active"));
    if (el) el.classList.add("active");

    chatWith.style.display = "flex";
    noChat.style.display = "none";
    msgInput.focus();

    // Show invite button only for channels
    inviteBtn.style.display = type === "channel" ? "" : "none";

    // Reset and start auto-refresh
    if (refreshInterval) clearInterval(refreshInterval);
    refreshInterval = setInterval(refreshActiveConvo, 5000);

    if (type === "dm") {
        chatHeader.textContent = id;
        chatHeaderInfo.textContent = "";
        if (!peerPublicKeys[id]) {
            const res = await fetch(`/api/users/${id}/public_key?token=${token}`);
            const data = await res.json();
            peerPublicKeys[id] = CRYPTO.importPublicKey(data.public_key_jwk);
        }
        await loadDmHistory(id);
    } else {
        const ch = channelsList.find(c => c.id === id);
        chatHeader.textContent = "# " + (ch ? ch.name : id);
        chatHeaderInfo.textContent = ch ? `${ch.members.length} members` : "";
        await loadChannelHistory(id);
    }
}

// --- Auto-refresh ---

async function refreshActiveConvo() {
    if (!activeConvo || !token) return;

    const key = convoKey(activeConvo.type, activeConvo.id);
    const cached = messageCache[key] || [];
    const lastTs = cached.length > 0 ? Math.max(...cached.map(m => m.timestamp || 0)) : 0;

    try {
        let msgs;
        if (activeConvo.type === "dm") {
            const res = await fetch(`/api/messages/${activeConvo.id}?token=${token}&after=${lastTs}`);
            msgs = await res.json();
        } else {
            const res = await fetch(`/api/channels/${activeConvo.id}/messages?token=${token}&after=${lastTs}`);
            msgs = await res.json();
        }

        if (!msgs.length) return;

        const existingIds = new Set(cached.map(m => m.id));
        const atBottom = messagesDiv.scrollTop + messagesDiv.clientHeight >= messagesDiv.scrollHeight - 50;
        let added = false;

        for (const m of msgs) {
            if (existingIds.has(m.id)) continue;

            // Skip if this is our own message and we already have a pending entry
            if (m.sender === username) {
                const pending = cached.find(c => c.id && String(c.id).startsWith("pending-"));
                if (pending) {
                    pending.id = m.id; // Replace pending id so future dedup works
                    continue;
                }
            }

            let text = null, imageUrl = null;
            try {
                if (m.message_type === "image" && m.attachment_id) {
                    const result = await decryptChannelImage(m);
                    text = result.text;
                    imageUrl = result.imageUrl;
                } else {
                    const plaintext = await CRYPTO.decrypt(m.ciphertext, m.iv, m.sender_public_key_jwk, privateKey);
                    const parsed = parseDecryptedContent(plaintext);
                    text = parsed.text;
                    imageUrl = parsed.imageUrl;
                }
            } catch (e) {
                text = "[unable to decrypt]";
            }
            const entry = { id: m.id, sender: m.sender, text, imageUrl, timestamp: m.timestamp };
            if (!messageCache[key]) messageCache[key] = [];
            messageCache[key].push(entry);
            renderMessage(entry);
            added = true;
        }

        if (added && atBottom) {
            messagesDiv.scrollTop = messagesDiv.scrollHeight;
        }
    } catch (e) {
        // Silently ignore refresh errors
    }
}

// --- DM history ---

async function loadDmHistory(peer) {
    const key = convoKey("dm", peer);
    messagesDiv.innerHTML = "";

    // Always fetch from server (cache is used for incremental refresh between full loads)
    messageCache[key] = [];

    const res = await fetch(`/api/messages/${peer}?token=${token}`);
    const msgs = await res.json();

    for (const m of msgs) {
        let text = null, imageUrl = null;
        try {
            const plaintext = await CRYPTO.decrypt(m.ciphertext, m.iv, m.sender_public_key_jwk, privateKey);
            const parsed = parseDecryptedContent(plaintext);
            text = parsed.text;
            imageUrl = parsed.imageUrl;
        } catch (e) {
            text = "[unable to decrypt]";
        }
        const entry = { id: m.id, sender: m.sender, text, imageUrl, timestamp: m.timestamp };
        messageCache[key].push(entry);
        renderMessage(entry);
    }
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
}

// --- Channel history ---

async function loadChannelHistory(channelId) {
    const key = convoKey("channel", channelId);
    messagesDiv.innerHTML = "";

    // Always fetch from server (cache is used for incremental refresh between full loads)
    messageCache[key] = [];

    const res = await fetch(`/api/channels/${channelId}/messages?token=${token}`);
    const msgs = await res.json();

    for (const m of msgs) {
        let text = null, imageUrl = null;
        try {
            if (m.message_type === "image" && m.attachment_id) {
                // Channel image: decrypt symmetric key, fetch blob, decrypt
                const result = await decryptChannelImage(m);
                text = result.text;
                imageUrl = result.imageUrl;
            } else {
                const plaintext = await CRYPTO.decrypt(m.ciphertext, m.iv, m.sender_public_key_jwk, privateKey);
                const parsed = parseDecryptedContent(plaintext);
                text = parsed.text;
                imageUrl = parsed.imageUrl;
            }
        } catch (e) {
            text = "[unable to decrypt]";
        }
        const entry = { id: m.id, sender: m.sender, text, imageUrl, timestamp: m.timestamp };
        messageCache[key].push(entry);
        renderMessage(entry);
    }
    messagesDiv.scrollTop = messagesDiv.scrollHeight;
}

// --- Decrypt helpers ---

/**
 * Parse decrypted plaintext — could be plain text or a JSON image envelope.
 * Returns { text, imageUrl } where one is null.
 */
function parseDecryptedContent(plaintext) {
    if (plaintext.startsWith("{")) {
        try {
            const parsed = JSON.parse(plaintext);
            if (parsed.type === "image" && parsed.data) {
                return { text: null, imageUrl: `data:${parsed.mime || "image/png"};base64,${parsed.data}` };
            }
        } catch {}
    }
    return { text: plaintext, imageUrl: null };
}

/**
 * Decrypt a channel image message: decrypt the symmetric key from ciphertext,
 * fetch the attachment blob, decrypt with the symmetric key.
 */
async function decryptChannelImage(msg) {
    // msg.ciphertext contains the encrypted symmetric key
    const keyB64 = await CRYPTO.decrypt(msg.ciphertext, msg.iv, msg.sender_public_key_jwk, privateKey);
    const key = sodium.from_base64(keyB64, sodium.base64_variants.ORIGINAL);

    // Fetch the encrypted blob
    const res = await fetch(`/api/attachments/${msg.attachment_id}?token=${token}`);
    if (!res.ok) throw new Error("Failed to fetch attachment");
    const att = await res.json();

    // Decrypt the blob with the symmetric key
    const plainBytes = await CRYPTO.decryptSymmetric(att.encrypted_data, att.iv, key);
    const plaintext = sodium.to_string(plainBytes);
    return parseDecryptedContent(plaintext);
}

// --- Render ---

function renderMessage(msg) {
    const div = document.createElement("div");
    div.className = "msg " + (msg.sender === username ? "sent" : "received");

    if (activeConvo && activeConvo.type === "channel" && msg.sender !== username) {
        const senderSpan = document.createElement("div");
        senderSpan.className = "msg-sender";
        senderSpan.textContent = msg.sender;
        div.appendChild(senderSpan);
    }

    if (msg.imageUrl) {
        const img = document.createElement("img");
        img.className = "msg-image";
        img.src = msg.imageUrl;
        img.onclick = () => window.open(img.src, "_blank");
        div.appendChild(img);
    } else {
        const textSpan = document.createElement("div");
        textSpan.textContent = msg.text;
        div.appendChild(textSpan);
    }

    const timeSpan = document.createElement("div");
    timeSpan.className = "msg-time";
    timeSpan.textContent = new Date(msg.timestamp * 1000).toLocaleTimeString()
        + (msg.sender === username ? " \u00b7 Sent" : "");
    div.appendChild(timeSpan);

    messagesDiv.appendChild(div);
}

// --- WebSocket ---

function connectWs() {
    if (ws) { ws.close(); ws = null; }
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    ws = new WebSocket(`${proto}//${location.host}/ws/chat?token=${token}`);

    ws.onmessage = async (event) => {
        const data = JSON.parse(event.data);
        if (data.ack) return;
        if (data.type === "channel") {
            await handleIncomingChannelMsg(data);
        } else {
            await handleIncomingDm(data);
        }
    };

    ws.onclose = () => {
        if (token) setTimeout(connectWs, 2000);
    };
}

async function handleIncomingDm(data) {
    if (data.sender === username) return;
    const peer = data.sender;
    const key = convoKey("dm", peer);
    if (messageCache[key] && messageCache[key].some(m => m.id === data.id)) return;
    let text = null, imageUrl = null;
    try {
        const plaintext = await CRYPTO.decrypt(data.ciphertext, data.iv, data.sender_public_key_jwk, privateKey);
        const parsed = parseDecryptedContent(plaintext);
        text = parsed.text;
        imageUrl = parsed.imageUrl;
    } catch (e) {
        text = "[unable to decrypt]";
    }
    const entry = { id: data.id, sender: data.sender, text, imageUrl, timestamp: data.timestamp };
    if (!messageCache[key]) messageCache[key] = [];
    messageCache[key].push(entry);
    if (activeConvo && activeConvo.type === "dm" && activeConvo.id === peer) {
        renderMessage(entry);
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
    }
}

async function handleIncomingChannelMsg(data) {
    // Skip our own messages — we already show them via the pending entry
    if (data.sender === username) return;

    const key = convoKey("channel", data.channel_id);
    if (messageCache[key] && messageCache[key].some(m => m.id === data.id)) return;
    let text = null, imageUrl = null;
    try {
        if (data.message_type === "image" && data.attachment_id) {
            const result = await decryptChannelImage(data);
            text = result.text;
            imageUrl = result.imageUrl;
        } else {
            const plaintext = await CRYPTO.decrypt(data.ciphertext, data.iv, data.sender_public_key_jwk, privateKey);
            const parsed = parseDecryptedContent(plaintext);
            text = parsed.text;
            imageUrl = parsed.imageUrl;
        }
    } catch (e) {
        text = "[unable to decrypt]";
    }
    const entry = { id: data.id, sender: data.sender, text, imageUrl, timestamp: data.timestamp, groupId: data.group_id };
    if (!messageCache[key]) messageCache[key] = [];
    if (data.group_id && messageCache[key].some(m => m.groupId === data.group_id)) return;
    messageCache[key].push(entry);
    if (activeConvo && activeConvo.type === "channel" && activeConvo.id === data.channel_id) {
        renderMessage(entry);
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
    }
}

// --- Image handling ---

function clearPendingImage() {
    pendingImage = null;
    const preview = document.getElementById("image-preview");
    if (preview) preview.style.display = "none";
}

function showImagePreview(file) {
    const preview = document.getElementById("image-preview");
    const img = document.getElementById("image-preview-img");
    const name = document.getElementById("image-preview-name");
    const reader = new FileReader();
    reader.onload = () => {
        img.src = reader.result;
        name.textContent = file.name;
        preview.style.display = "flex";
    };
    reader.readAsDataURL(file);
}

function handleImageSelect(file) {
    if (!file || !file.type.startsWith("image/")) return;
    if (file.size > MAX_IMAGE_SIZE) {
        alert(`Image too large. Maximum size is ${MAX_IMAGE_SIZE / 1024 / 1024}MB.`);
        return;
    }
    file.arrayBuffer().then(buf => {
        pendingImage = { data: new Uint8Array(buf), mime: file.type, name: file.name };
        showImagePreview(file);
    });
}

// --- Sending ---

async function sendMessage() {
    if (!activeConvo) return;
    if (pendingImage) {
        if (activeConvo.type === "dm") {
            await sendDmImage();
        } else {
            await sendChannelImage();
        }
        clearPendingImage();
        return;
    }
    if (!msgInput.value.trim()) return;
    const text = msgInput.value.trim();
    msgInput.value = "";
    if (activeConvo.type === "dm") {
        await sendDm(text);
    } else {
        await sendChannelMsg(text);
    }
}

async function sendDm(text) {
    const peer = activeConvo.id;
    const recipientPub = peerPublicKeys[peer];
    if (!recipientPub) return;

    const encForRecipient = await CRYPTO.encrypt(text, recipientPub);
    const myPub = CRYPTO.importPublicKey(publicKeyB64);
    const encForSelf = await CRYPTO.encrypt(text, myPub);

    ws.send(JSON.stringify({
        type: "dm",
        recipient: peer,
        ciphertext: encForRecipient.ciphertext,
        iv: encForRecipient.iv,
        sender_public_key_jwk: encForRecipient.ephemeralPublicKey,
        self_ciphertext: encForSelf.ciphertext,
        self_iv: encForSelf.iv,
        self_sender_public_key_jwk: encForSelf.ephemeralPublicKey,
    }));

    const key = convoKey("dm", peer);
    const entry = { id: "pending-" + Date.now(), sender: username, text, timestamp: Date.now() / 1000 };
    if (!messageCache[key]) messageCache[key] = [];
    messageCache[key].push(entry);
    renderMessage(entry);
    messagesDiv.scrollTop = messagesDiv.scrollHeight;

    if (!allUsers.some(u => u.username === peer)) {
        allUsers.push({ username: peer });
        renderUserList();
    }
}

async function sendDmImage() {
    const peer = activeConvo.id;
    const recipientPub = peerPublicKeys[peer];
    if (!recipientPub || !pendingImage) return;

    // Pack image into JSON envelope then encrypt
    const envelope = JSON.stringify({
        type: "image",
        mime: pendingImage.mime,
        data: sodium.to_base64(pendingImage.data, sodium.base64_variants.ORIGINAL),
    });

    const encForRecipient = await CRYPTO.encrypt(envelope, recipientPub);
    const myPub = CRYPTO.importPublicKey(publicKeyB64);
    const encForSelf = await CRYPTO.encrypt(envelope, myPub);

    ws.send(JSON.stringify({
        type: "dm",
        recipient: peer,
        ciphertext: encForRecipient.ciphertext,
        iv: encForRecipient.iv,
        sender_public_key_jwk: encForRecipient.ephemeralPublicKey,
        self_ciphertext: encForSelf.ciphertext,
        self_iv: encForSelf.iv,
        self_sender_public_key_jwk: encForSelf.ephemeralPublicKey,
        message_type: "image",
    }));

    const key = convoKey("dm", peer);
    const imgSrc = `data:${pendingImage.mime};base64,${sodium.to_base64(pendingImage.data, sodium.base64_variants.ORIGINAL)}`;
    const entry = { id: "pending-" + Date.now(), sender: username, text: null, imageUrl: imgSrc, timestamp: Date.now() / 1000 };
    if (!messageCache[key]) messageCache[key] = [];
    messageCache[key].push(entry);
    renderMessage(entry);
    messagesDiv.scrollTop = messagesDiv.scrollHeight;

    if (!allUsers.some(u => u.username === peer)) {
        allUsers.push({ username: peer });
        renderUserList();
    }
}

async function sendChannelImage() {
    if (!pendingImage) return;
    const channelId = activeConvo.id;
    try {
        const res = await fetch(`/api/channels/${channelId}/members/public_keys?token=${token}`);
        if (!res.ok) throw new Error("Failed to fetch member keys");
        const memberKeys = await res.json();

        // Pack image into JSON envelope
        const envelope = JSON.stringify({
            type: "image",
            mime: pendingImage.mime,
            data: sodium.to_base64(pendingImage.data, sodium.base64_variants.ORIGINAL),
        });

        // Encrypt envelope once with symmetric key
        const envelopeBytes = sodium.from_string(envelope);
        const sym = await CRYPTO.encryptSymmetric(envelopeBytes);

        // Encrypt the symmetric key for each member
        const keyB64 = sodium.to_base64(sym.key, sodium.base64_variants.ORIGINAL);
        const envelopes = [];
        for (const mk of memberKeys) {
            const pubKey = CRYPTO.importPublicKey(mk.public_key_jwk);
            const encrypted = await CRYPTO.encrypt(keyB64, pubKey);
            envelopes.push({
                target_user: mk.username,
                ciphertext: encrypted.ciphertext,
                iv: encrypted.iv,
                sender_public_key_jwk: encrypted.ephemeralPublicKey,
            });
        }

        ws.send(JSON.stringify({
            type: "channel",
            channel_id: channelId,
            envelopes,
            message_type: "image",
            attachment: {
                encrypted_data: sym.ciphertext,
                iv: sym.iv,
            },
        }));

        const key = convoKey("channel", channelId);
        const imgSrc = `data:${pendingImage.mime};base64,${sodium.to_base64(pendingImage.data, sodium.base64_variants.ORIGINAL)}`;
        const entry = { id: "pending-" + Date.now(), sender: username, text: null, imageUrl: imgSrc, timestamp: Date.now() / 1000 };
        if (!messageCache[key]) messageCache[key] = [];
        messageCache[key].push(entry);
        renderMessage(entry);
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
    } catch (e) {
        console.error("Failed to send image:", e);
    }
}

async function sendChannelMsg(text) {
    const channelId = activeConvo.id;
    try {
        const res = await fetch(`/api/channels/${channelId}/members/public_keys?token=${token}`);
        if (!res.ok) throw new Error("Failed to fetch member keys");
        const memberKeys = await res.json();
        const envelopes = [];
        for (const mk of memberKeys) {
            const pubKey = CRYPTO.importPublicKey(mk.public_key_jwk);
            const encrypted = await CRYPTO.encrypt(text, pubKey);
            envelopes.push({
                target_user: mk.username,
                ciphertext: encrypted.ciphertext,
                iv: encrypted.iv,
                sender_public_key_jwk: encrypted.ephemeralPublicKey,
            });
        }
        ws.send(JSON.stringify({
            type: "channel",
            channel_id: channelId,
            envelopes: envelopes,
        }));

        // Show message immediately (server will relay back, deduped by group_id)
        const key = convoKey("channel", channelId);
        const entry = { id: "pending-" + Date.now(), sender: username, text, timestamp: Date.now() / 1000 };
        if (!messageCache[key]) messageCache[key] = [];
        messageCache[key].push(entry);
        renderMessage(entry);
        messagesDiv.scrollTop = messagesDiv.scrollHeight;
    } catch (e) {
        // Re-insert text so user can retry
        msgInput.value = text;
    }
}

// --- Channel creation modal ---

function openCreateChannelModal() {
    modalError.textContent = "";
    channelNameInput.value = "";
    memberCheckboxes.innerHTML = "";
    for (const u of allUsers) {
        const label = document.createElement("label");
        label.className = "member-checkbox-label";
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.value = u.username;
        label.appendChild(cb);
        label.appendChild(document.createTextNode(" " + u.username));
        memberCheckboxes.appendChild(label);
    }
    modalOverlay.style.display = "flex";
}

function closeModal() { modalOverlay.style.display = "none"; }

async function createChannel() {
    const name = channelNameInput.value.trim();
    if (!name) { modalError.textContent = "Channel name is required"; return; }
    const checked = memberCheckboxes.querySelectorAll("input:checked");
    const members = Array.from(checked).map(cb => cb.value);
    const res = await fetch(`/api/channels?token=${token}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name, members }),
    });
    if (!res.ok) {
        let msg = "Failed to create channel";
        try { const err = await res.json(); msg = err.detail || msg; } catch {}
        modalError.textContent = msg;
        return;
    }
    const ch = await res.json();
    channelsList.push(ch);
    renderChannelList();
    closeModal();
}

// --- Browse channels modal ---

async function openBrowseChannelsModal() {
    const modal = document.getElementById("browse-channels-modal");
    const list = document.getElementById("browse-channels-list");
    list.innerHTML = "Loading...";
    modal.style.display = "flex";

    try {
        const res = await fetch(`/api/channels/browse?token=${token}`);
        const channels = await res.json();

        if (channels.length === 0) {
            list.innerHTML = '<div style="color:#888;padding:0.5rem">No channels available to join</div>';
            return;
        }

        list.innerHTML = "";
        for (const ch of channels) {
            const row = document.createElement("div");
            row.className = "browse-channel-row";
            row.innerHTML = `<div><strong># ${ch.name}</strong> <span style="color:#666;font-size:0.8rem">${ch.member_count} members</span></div>`;
            const btn = document.createElement("button");
            btn.className = "btn-primary";
            btn.style.fontSize = "0.75rem";
            btn.style.padding = "0.3rem 0.8rem";
            btn.textContent = "Join";
            btn.onclick = async () => {
                btn.disabled = true;
                btn.textContent = "Joining...";
                try {
                    const joinRes = await fetch(`/api/channels/${ch.id}/join?token=${token}`, { method: "POST" });
                    if (!joinRes.ok) {
                        let msg = "Failed to join";
                        try { const err = await joinRes.json(); msg = err.detail || msg; } catch {}
                        btn.textContent = msg;
                        return;
                    }
                    const joined = await joinRes.json();
                    channelsList.push(joined);
                    renderChannelList();
                    btn.textContent = "Joined";
                } catch (e) {
                    btn.textContent = "Error";
                }
            };
            row.appendChild(btn);
            list.appendChild(row);
        }
    } catch (e) {
        list.innerHTML = '<div style="color:#ff6b6b;padding:0.5rem">Failed to load channels</div>';
    }
}

// --- Invite modal ---

async function openInviteModal() {
    if (!activeConvo || activeConvo.type !== "channel") return;

    const modal = document.getElementById("invite-modal");
    const checkboxes = document.getElementById("invite-checkboxes");
    const error = document.getElementById("invite-error");
    error.textContent = "";
    checkboxes.innerHTML = "";

    const ch = channelsList.find(c => c.id === activeConvo.id);
    if (!ch) return;

    // Refresh user list in case new users registered
    try {
        const usersRes = await fetch(`/api/users?token=${token}`);
        allUsers = await usersRes.json();
    } catch (e) {}

    const memberSet = new Set(ch.members);
    const nonMembers = allUsers.filter(u => !memberSet.has(u.username));

    if (nonMembers.length === 0) {
        checkboxes.innerHTML = '<div style="color:#888;padding:0.5rem">All users are already members</div>';
        modal.style.display = "flex";
        return;
    }

    for (const u of nonMembers) {
        const label = document.createElement("label");
        label.className = "member-checkbox-label";
        const cb = document.createElement("input");
        cb.type = "checkbox";
        cb.value = u.username;
        label.appendChild(cb);
        label.appendChild(document.createTextNode(" " + u.username));
        checkboxes.appendChild(label);
    }
    modal.style.display = "flex";
}

async function inviteToChannel() {
    if (!activeConvo || activeConvo.type !== "channel") return;

    const checkboxes = document.getElementById("invite-checkboxes");
    const error = document.getElementById("invite-error");
    const checked = checkboxes.querySelectorAll("input:checked");
    const users = Array.from(checked).map(cb => cb.value);

    if (users.length === 0) {
        error.textContent = "Select at least one user";
        return;
    }

    error.textContent = "";
    const channelId = activeConvo.id;

    for (const u of users) {
        try {
            const res = await fetch(`/api/channels/${channelId}/members?token=${token}`, {
                method: "POST",
                headers: { "Content-Type": "application/json" },
                body: JSON.stringify({ username: u }),
            });
            if (!res.ok) {
                const err = await res.json();
                error.textContent = `Failed to invite ${u}: ${err.detail || "error"}`;
                return;
            }
        } catch (e) {
            error.textContent = `Failed to invite ${u}`;
            return;
        }
    }

    // Refresh channel info
    const ch = channelsList.find(c => c.id === channelId);
    if (ch) {
        ch.members = ch.members.concat(users);
        chatHeaderInfo.textContent = `${ch.members.length} members`;
    }

    document.getElementById("invite-modal").style.display = "none";
}

// --- Event listeners ---
document.getElementById("btn-register").onclick = register;
document.getElementById("btn-login").onclick = login;
document.getElementById("btn-logout").onclick = logout;
document.getElementById("btn-create-channel").onclick = openCreateChannelModal;
document.getElementById("btn-browse-channels").onclick = openBrowseChannelsModal;
document.getElementById("btn-browse-close").onclick = () => { document.getElementById("browse-channels-modal").style.display = "none"; };
document.getElementById("btn-modal-cancel").onclick = closeModal;
document.getElementById("btn-modal-create").onclick = createChannel;
inviteBtn.onclick = openInviteModal;
document.getElementById("btn-invite-cancel").onclick = () => { document.getElementById("invite-modal").style.display = "none"; };
document.getElementById("btn-invite-confirm").onclick = inviteToChannel;
sendBtn.onclick = sendMessage;
msgInput.addEventListener("keydown", (e) => { if (e.key === "Enter") sendMessage(); });

// Image attachment
document.getElementById("image-input").onchange = (e) => { if (e.target.files[0]) handleImageSelect(e.target.files[0]); e.target.value = ""; };
document.getElementById("image-preview-cancel").onclick = clearPendingImage;
passwordInput.addEventListener("keydown", (e) => { if (e.key === "Enter") login(); });
channelNameInput.addEventListener("keydown", (e) => { if (e.key === "Enter") createChannel(); });

// Key management
const keyFileInput = document.getElementById("key-file-input");
document.getElementById("btn-import-keys").onclick = () => keyFileInput.click();
keyFileInput.onchange = (e) => { if (e.target.files[0]) importKeysFromFile(e.target.files[0]); };
document.getElementById("btn-export-keys-auth").onclick = async () => {
    const user = usernameInput.value.trim();
    if (!user) { showAuthError("Enter a username first.", false); return; }
    await exportKeysToFile(user);
};
document.getElementById("btn-export-keys").onclick = async () => { if (username) await exportKeysToFile(username); };

// Key password modal
document.getElementById("btn-key-password-ok").onclick = () => {
    const pw = keyPasswordInput.value;
    if (!pw) { keyPasswordError.textContent = "Password required"; return; }
    closeKeyPasswordModal(pw);
};
document.getElementById("btn-key-password-cancel").onclick = () => closeKeyPasswordModal(null);
keyPasswordInput.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
        const pw = keyPasswordInput.value;
        if (!pw) { keyPasswordError.textContent = "Password required"; return; }
        closeKeyPasswordModal(pw);
    }
});

// DM user search
let dmSearchTimeout = null;
const dmSearchInput = document.getElementById("dm-search");
const dmSearchResults = document.getElementById("dm-search-results");
dmSearchInput.addEventListener("input", () => {
    clearTimeout(dmSearchTimeout);
    const q = dmSearchInput.value.trim();
    if (!q) { dmSearchResults.innerHTML = ""; return; }
    dmSearchTimeout = setTimeout(async () => {
        try {
            const res = await fetch(`/api/users/search?q=${encodeURIComponent(q)}&token=${token}`);
            const users = await res.json();
            dmSearchResults.innerHTML = "";
            for (const u of users) {
                const div = document.createElement("div");
                div.className = "sidebar-item";
                div.textContent = u.username;
                div.onclick = () => {
                    dmSearchInput.value = "";
                    dmSearchResults.innerHTML = "";
                    selectConvo("dm", u.username, null);
                };
                dmSearchResults.appendChild(div);
            }
        } catch (e) {}
    }, 300);
});
