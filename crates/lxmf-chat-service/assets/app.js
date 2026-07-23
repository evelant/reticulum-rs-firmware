"use strict";

const state = {
  contacts: [],
  selected: null,
  draftKey: null,
  draftTimestamp: null,
  events: null,
};

const $ = (id) => document.getElementById(id);

function randomHex(bytes) {
  const data = new Uint8Array(bytes);
  crypto.getRandomValues(data);
  return Array.from(data, (value) => value.toString(16).padStart(2, "0")).join("");
}

async function api(path, options = {}) {
  const headers = new Headers(options.headers || {});
  if (options.body !== undefined) {
    headers.set("Content-Type", "application/json");
    headers.set("X-Reticulum-Client", "web-alpha");
  }
  const response = await fetch(path, { ...options, headers, credentials: "same-origin" });
  if (!response.ok) {
    let detail = `${response.status} ${response.statusText}`;
    try {
      const body = await response.json();
      detail = body.error || detail;
    } catch (_) {}
    throw new Error(detail);
  }
  if (response.status === 204) return null;
  return response.json();
}

function showError(error) {
  const element = $("error");
  if (!error) {
    element.textContent = "";
    element.classList.add("hidden");
    return;
  }
  element.textContent = error instanceof Error ? error.message : String(error);
  element.classList.remove("hidden");
}

async function bootstrap() {
  const fragment = new URLSearchParams(location.hash.slice(1));
  const capability = fragment.get("cap");
  if (capability) {
    await api("/api/v1/session", {
      method: "POST",
      body: JSON.stringify({ capability }),
    });
    history.replaceState(null, "", `${location.pathname}${location.search}`);
  }
}

function renderSnapshot(snapshot) {
  const connection = snapshot.connection;
  const pill = $("connection");
  pill.textContent = connection.state.replaceAll("_", " ");
  pill.className = `pill ${connection.state}`;
  $("pending-count").textContent = snapshot.pending_outbox;
  $("imported-count").textContent = snapshot.imported_this_run;
  $("local-destination").textContent = snapshot.device?.lxmf_delivery_destination || "—";
  if (snapshot.last_error) showError(snapshot.last_error);
  else showError(null);
}

function renderContacts() {
  const list = $("contacts");
  list.replaceChildren();
  for (const contact of state.contacts) {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `contact${state.selected === contact.destination ? " active" : ""}`;
    const name = document.createElement("strong");
    name.textContent = contact.name || "Unnamed contact";
    const hash = document.createElement("small");
    hash.textContent = contact.destination;
    button.append(name, hash);
    button.addEventListener("click", () => selectContact(contact.destination));
    list.append(button);
  }
}

function bytesText(field) {
  return field.encoding === "utf8" ? field.value : `hex:${field.value}`;
}

async function renderTimeline() {
  if (!state.selected) return;
  const entries = await api(`/api/v1/conversations/${state.selected}`);
  const timeline = $("timeline");
  timeline.replaceChildren();
  for (const entry of entries) {
    const item = document.createElement("li");
    item.className = `message ${entry.direction}`;
    const title = document.createElement("h3");
    title.textContent = bytesText(entry.title) || "Untitled";
    const content = document.createElement("p");
    content.textContent = bytesText(entry.content);
    const footer = document.createElement("footer");
    const time = new Date(entry.timestamp_ms).toLocaleString();
    footer.textContent = entry.status ? `${time} · ${entry.status.replaceAll("_", " ")}` : time;
    item.append(title, content, footer);
    timeline.append(item);
  }
  timeline.scrollTop = timeline.scrollHeight;
}

async function selectContact(destination) {
  state.selected = destination;
  state.draftKey = null;
  state.draftTimestamp = null;
  renderContacts();
  const contact = state.contacts.find((item) => item.destination === destination);
  $("peer-name").textContent = contact?.name || "Unnamed contact";
  $("peer-destination").textContent = destination;
  $("empty-state").classList.add("hidden");
  $("active-conversation").classList.remove("hidden");
  await renderTimeline();
}

async function refresh() {
  const [snapshot, contacts] = await Promise.all([
    api("/api/v1/snapshot"),
    api("/api/v1/contacts"),
  ]);
  renderSnapshot(snapshot);
  state.contacts = contacts;
  if (state.selected && !contacts.some((item) => item.destination === state.selected)) {
    state.selected = null;
  }
  renderContacts();
  if (state.selected) await renderTimeline();
}

function startEvents() {
  state.events?.close();
  const events = new EventSource("/api/v1/events");
  events.addEventListener("invalidate", () => refresh().catch(showError));
  events.onerror = () => {
    $("connection").textContent = "event stream reconnecting";
  };
  state.events = events;
}

$("show-contact-form").addEventListener("click", () => $("contact-form").classList.remove("hidden"));
$("cancel-contact").addEventListener("click", () => $("contact-form").classList.add("hidden"));
$("contact-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  try {
    const destination = $("contact-destination").value.trim().toLowerCase();
    await api(`/api/v1/contacts/${destination}`, {
      method: "PUT",
      body: JSON.stringify({ name: $("contact-name").value }),
    });
    event.target.reset();
    event.target.classList.add("hidden");
    await refresh();
    await selectContact(destination);
  } catch (error) { showError(error); }
});

$("message-content").addEventListener("input", () => {
  $("message-length").textContent = `${new TextEncoder().encode($("message-content").value).length} / 295`;
  state.draftKey = null;
  state.draftTimestamp = null;
});
$("message-title").addEventListener("input", () => {
  state.draftKey = null;
  state.draftTimestamp = null;
});

$("compose").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (!state.selected) return;
  if (!state.draftKey) {
    state.draftKey = randomHex(16);
    state.draftTimestamp = Date.now();
  }
  try {
    await api("/api/v1/messages", {
      method: "POST",
      body: JSON.stringify({
        destination: state.selected,
        timestamp_ms: state.draftTimestamp,
        idempotency_key: state.draftKey,
        title: $("message-title").value,
        content: $("message-content").value,
      }),
    });
    state.draftKey = null;
    state.draftTimestamp = null;
    event.target.reset();
    $("message-length").textContent = "0 / 295";
    await refresh();
  } catch (error) { showError(error); }
});

$("sync").addEventListener("click", () => api("/api/v1/sync", { method: "POST", body: "{}" }).catch(showError));
$("reconnect").addEventListener("click", () => api("/api/v1/reconnect", { method: "POST", body: "{}" }).catch(showError));

(async () => {
  try {
    await bootstrap();
    await refresh();
    startEvents();
  } catch (error) {
    showError(error);
  }
})();
