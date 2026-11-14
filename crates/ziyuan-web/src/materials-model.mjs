// Form state only: no DOM, network client, or temporary preview associations.
const clone = (value) => JSON.parse(JSON.stringify(value));
// JSON-semantic equality: the server serializes fields in its own declaration order.
function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort()
      .filter((key) => value[key] !== undefined)
      .map((key) => [key, canonical(value[key])]));
  }
  return value;
}
const same = (left, right) => JSON.stringify(canonical(left)) === JSON.stringify(canonical(right));

// The server stores null in an Option field as None and omits it on GET
// (skip_serializing_if). Only those fields are normalized; nulls elsewhere stay distinct.
const isObject = (value) => value !== null && typeof value === "object" && !Array.isArray(value);
function withoutNone(value, fields, nested = {}) {
  if (!isObject(value)) return value;
  const copy = { ...value };
  for (const field of fields) if (copy[field] === null) delete copy[field];
  for (const [field, normalize] of Object.entries(nested)) {
    if (Object.hasOwn(copy, field)) copy[field] = normalize(copy[field]);
  }
  return copy;
}
const mapArray = (value, normalize) => (Array.isArray(value) ? value.map(normalize) : value);
const position = (value) => withoutNone(value, ["paragraph"]);
const scope = (value) => (value?.kind === "range"
  ? withoutNone(value, [], { from: position, to: position }) : value);
const term = (value) => withoutNone(value, ["note", "available_from"], {
  available_from: position, scope,
});
const rule = (value) => withoutNone(value, ["note"], { scope });
const person = (value) => withoutNone(value, ["available_from", "reading"], {
  available_from: position,
  voice: (voice) => withoutNone(voice, ["examples"]),
  address_rules: (rules) => mapArray(rules, rule),
});
const wireMaterial = (value) => withoutNone(value, [], {
  terms: (terms) => mapArray(terms, term), persons: (persons) => mapArray(persons, person),
});
const sameMaterial = (left, right) => same(wireMaterial(left), wireMaterial(right));
const emptyMaterial = () => ({ format: 2, terms: [], persons: [] });
const isMaterial = (value) => value?.format === 2
  && Array.isArray(value.terms) && Array.isArray(value.persons);
const isRevision = (value) => value === null || typeof value === "string";

function defaultRequestId() {
  if (globalThis.crypto.randomUUID) return globalThis.crypto.randomUUID();
  return Array.from(globalThis.crypto.getRandomValues(new Uint8Array(16)),
    (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function freeze(value) {
  if (value && typeof value === "object") {
    Object.values(value).forEach(freeze);
    Object.freeze(value);
  }
  return value;
}

/**
 * receive() accepts the materials GET response (workspace_id, project, draft).
 * prepareSave() returns an immutable, directly POST-able request. Keep that
 * request for acceptSave(); failed transports need no state transition.
 * needsRefresh/refresh notify the caller that a receipt is not a current view.
 * Network callers must take beginReceive() before issuing each GET and pass the
 * ticket to receive(view, ticket): revisions are opaque, so only the ticket can
 * reveal that a response was overtaken by a later GET or save acknowledgement.
 * receive(view) without a ticket is for synchronous use and is applied as newest.
 */
export class MaterialModel {
  constructor({
    project, origin = globalThis.location?.origin ?? "", module = "materials",
    storage, requestId = defaultRequestId, refresh = () => {},
    // Browser quota is authoritative; raw nulls can exceed the server's compact size.
    maxMirrorBytes = Number.POSITIVE_INFINITY,
  }) {
    this.project = project;
    this.origin = origin;
    this.module = module;
    this.requestId = requestId;
    this.refresh = refresh;
    this.maxMirrorBytes = maxMirrorBytes;
    this.workspaceId = null;
    this.material = null;
    this.remoteRevision = null;
    this.savedSnapshot = null;
    this.pendingSave = null;
    this.localEpoch = 0;
    this.projectHash = null;
    this.validation = null;
    this.preview = null;
    this.mirrorError = null;
    this.recoveryChoice = null;
    this.needsRefresh = false;
    this._baselineRevision = null;
    this._remoteMaterial = null;
    this._requests = new Map();
    this._acknowledged = new Set();
    this._generation = 0;
    this._keys = new WeakMap();
    this._usedKeys = new Set();
    this._nextKey = 0;
    this._tickets = new WeakSet();
    this._ticketSequence = 0;
    this._receiveWatermark = 0;
    if (storage === undefined) {
      try { storage = globalThis.sessionStorage; } catch {
        this.mirrorError = "浏览器暂存不可用，当前输入仅保留在内存中。";
      }
    }
    this.storage = storage;
  }

  get canSave() { return this.material !== null && this.workspaceId !== null; }
  get conflict() { return this.canSave && this._baselineRevision !== this.remoteRevision; }
  get dirty() {
    return this.canSave && (this.conflict || !sameMaterial(this.material, this.savedSnapshot));
  }
  get identity() {
    return {
      origin: this.origin, workspace_id: this.workspaceId,
      project: this.project, module: this.module,
    };
  }
  get mirrorKey() {
    // Array encoding is unambiguous even when a component contains separators.
    return this.workspaceId === null ? null
      : `ziyuan:material:v1:${JSON.stringify(Object.values(this.identity))}`;
  }
  get entryKeys() {
    const key = (entry) => {
      if (!this._keys.has(entry)) {
        let next;
        do { next = `entry-${++this._nextKey}`; } while (this._usedKeys.has(next));
        this._keys.set(entry, next);
        this._usedKeys.add(next);
      }
      return this._keys.get(entry);
    };
    return {
      terms: (this.material?.terms ?? []).map(key),
      persons: (this.material?.persons ?? []).map(key),
    };
  }

  beginReceive() {
    const ticket = Object.freeze({
      sequence: ++this._ticketSequence, generation: this._generation,
      workspace_id: this.workspaceId,
    });
    this._tickets.add(ticket);
    return ticket;
  }

  receive(view, ticket) {
    if (ticket !== undefined && (!this._tickets.has(ticket)
      || ticket.generation !== this._generation || ticket.workspace_id !== this.workspaceId
      || ticket.sequence < this._receiveWatermark)) {
      this.needsRefresh = true;
      this.refresh();
      return false;
    }
    if (typeof view.workspace_id !== "string" || !view.workspace_id
      || view.project?.id !== this.project
      || (view.draft && (view.draft.project !== this.project
        || typeof view.draft.revision !== "string" || !isMaterial(view.draft.material)))) {
      throw new Error("The materials view has missing or mismatched identity.");
    }
    const before = this._resultInputs();
    const changedWorkspace = this.workspaceId !== view.workspace_id;
    // An unconfirmed submission may be what this GET shows; a clean-looking local
    // state (e.g. an edit reverted after submit) must not be overwritten by it.
    const keepLocal = !changedWorkspace && (this.dirty || this._hasLiveSubmission());
    if (changedWorkspace) {
      this._generation++;
      this._requests.clear();
      this._acknowledged.clear();
      this.pendingSave = null;
      this.recoveryChoice = null;
      this.mirrorError = null;
    }
    this._receiveWatermark = ticket === undefined
      ? this._ticketSequence + 1 : Math.max(this._receiveWatermark, ticket.sequence + 1);
    this.workspaceId = view.workspace_id;
    this.projectHash = view.project_hash ?? null;
    this.remoteRevision = view.draft?.revision ?? null;
    this._remoteMaterial = clone(view.draft?.material ?? emptyMaterial());
    this.needsRefresh = false;
    if (!keepLocal) {
      if (changedWorkspace || !sameMaterial(this.material, this._remoteMaterial)) {
        this.material = clone(this._remoteMaterial);
      }
      this.savedSnapshot = clone(this._remoteMaterial);
      this._baselineRevision = this.remoteRevision;
      if (this.pendingSave && this.pendingSave.expected_revision !== this._baselineRevision) {
        this.pendingSave = null;
      }
      // A new workspace's mirror is read below and must not be overwritten first.
      if (!changedWorkspace) this._writeMirror();
    } else if (this._reconcile()) {
      this._writeMirror();
    }
    // Polls are often faster than validation/preview, so a duplicate view must not
    // invalidate results that still describe the same input and project.
    if (this._resultInputs() !== before) {
      this.localEpoch++;
      this.validation = null;
      this.invalidatePreview();
    }
    if (changedWorkspace) this._readMirror();
    return this;
  }

  edit(path, value) {
    if (!this.canSave) throw new Error("Load a materials view before editing.");
    if (!Array.isArray(path) || !path.length || path.some((part) =>
      !["string", "number"].includes(typeof part)
      || ["__proto__", "prototype", "constructor"].includes(part))) {
      throw new Error("Invalid material field path.");
    }
    let parent = this.material;
    for (const part of path.slice(0, -1)) {
      if (!Object.hasOwn(parent, part)) throw new Error("Unknown material field path.");
      parent = parent[part];
    }
    // Allocate keys before replacing array members so surviving objects retain theirs.
    this.entryKeys;
    const copied = clone(value);
    this._retainKeys(value, copied);
    parent[path.at(-1)] = copied;
    this.localEpoch++;
    this.validation = null;
    this.invalidatePreview();
    this._writeMirror();
  }

  prepareSave() {
    if (!this.canSave) throw new Error("Load a materials view before saving.");
    // A request built on a superseded baseline would only earn a permanent 409.
    if (this.pendingSave && sameMaterial(this.pendingSave.material, this.material)
      && this.pendingSave.expected_revision === this._baselineRevision) {
      return this.pendingSave;
    }
    const request = freeze({
      workspace_id: this.workspaceId, request_id: this.requestId(),
      expected_revision: this._baselineRevision, material: clone(this.material),
    });
    this.pendingSave = request;
    this._requests.set(request.request_id, { request, generation: this._generation });
    this._writeMirror();
    return request;
  }

  acceptSave(receipt, request) {
    const sent = this._requests.get(request?.request_id);
    if (!sent || !same(sent.request, request) || this._acknowledged.has(request.request_id)
      || request.workspace_id !== this.workspaceId
      || receipt?.workspace_id !== this.workspaceId
      || typeof receipt.revision !== "string" || !receipt.revision) return false;
    this._acknowledged.add(request.request_id);
    if (this.pendingSave?.request_id === request.request_id) this.pendingSave = null;
    // Revisions are opaque, not sortable. Only a receipt against the still-current
    // server baseline can advance it. Historical idempotency receipts require GET.
    // A GET may already have observed the revision this receipt produced.
    const observed = receipt.revision === this.remoteRevision
      && sameMaterial(request.material, this._remoteMaterial);
    if (sent.generation !== this._generation
      || (request.expected_revision !== this.remoteRevision && !observed)) {
      this.needsRefresh = true;
      this._writeMirror();
      this.refresh();
      return false;
    }
    // Every GET already in flight was issued before this commit was known.
    this._receiveWatermark = this._ticketSequence + 1;
    this.remoteRevision = receipt.revision;
    this._baselineRevision = receipt.revision;
    this.savedSnapshot = clone(request.material);
    this._remoteMaterial = clone(request.material);
    // Never assign request.material to material: edits made after submit survive.
    this.validation = null;
    this.invalidatePreview();
    this._writeMirror();
    return true;
  }

  restore() {
    if (!this.recoveryChoice) return false;
    const recovery = this.recoveryChoice;
    this.material = clone(recovery.material);
    this.savedSnapshot = clone(recovery.savedSnapshot);
    this._baselineRevision = recovery.baseRevision;
    this._restoreKeys(recovery.entryKeys);
    this.pendingSave = null;
    if (recovery.pendingSave) {
      const request = freeze(clone(recovery.pendingSave));
      this.pendingSave = request;
      this._requests.set(request.request_id, { request, generation: this._generation });
    }
    this.recoveryChoice = null;
    this._reconcile();
    this.localEpoch++;
    this.validation = null;
    this.invalidatePreview();
    this._writeMirror();
    return true;
  }

  discardLocalConfirmed() {
    if (!this.canSave) return;
    this.material = clone(this._remoteMaterial);
    this.savedSnapshot = clone(this._remoteMaterial);
    this._baselineRevision = this.remoteRevision;
    this.pendingSave = null;
    this.recoveryChoice = null;
    this._generation++;
    this.localEpoch++;
    this.validation = null;
    this.invalidatePreview();
    this._writeMirror();
  }

  invalidatePreview() { this.preview = null; }

  _resultInputs() {
    return JSON.stringify(canonical([
      this.workspaceId, this.projectHash, this.remoteRevision, this._baselineRevision,
      wireMaterial(this._remoteMaterial), wireMaterial(this.savedSnapshot),
      wireMaterial(this.material),
    ]));
  }

  // Only an unacknowledged request against the current baseline can still commit
  // content this model has not yet seen; a no-op request cannot change the input.
  _hasLiveSubmission() {
    return [...this._requests.values()].some((sent) => sent.generation === this._generation
      && !this._acknowledged.has(sent.request.request_id)
      && sent.request.expected_revision === this._baselineRevision
      && !sameMaterial(sent.request.material, this.material));
  }

  // When the server already holds the current input or a submitted snapshot (for
  // example a commit whose receipt is late or lost), rebase onto it instead of
  // reporting a conflict that only a discard could clear.
  _reconcile() {
    if (this._baselineRevision === this.remoteRevision) return false;
    const remote = this._remoteMaterial;
    const submitted = [this.pendingSave, ...[...this._requests.values()]
      .filter((sent) => sent.generation === this._generation
        && !this._acknowledged.has(sent.request.request_id))
      .map((sent) => sent.request)];
    if (!sameMaterial(this.material, remote)
      && !submitted.some((request) => request && sameMaterial(request.material, remote))) return false;
    this.savedSnapshot = clone(remote);
    this._baselineRevision = this.remoteRevision;
    if (this.pendingSave && sameMaterial(this.pendingSave.material, remote)) this.pendingSave = null;
    return true;
  }

  _retainKeys(original, copied) {
    if (!original || typeof original !== "object") return;
    const key = this._keys.get(original);
    if (key) this._keys.set(copied, key);
    Object.keys(original).forEach((field) => this._retainKeys(original[field], copied[field]));
  }

  _restoreKeys(keys) {
    this._keys = new WeakMap();
    this._usedKeys = new Set();
    for (const group of ["terms", "persons"]) {
      this.material[group].forEach((entry, index) => {
        const key = keys?.[group]?.[index];
        if (typeof key === "string" && !this._usedKeys.has(key)) {
          this._keys.set(entry, key);
          this._usedKeys.add(key);
        }
      });
    }
  }

  _readMirror() {
    try {
      if (!this.storage) throw new Error("Storage unavailable");
      const raw = this.storage.getItem(this.mirrorKey);
      if (raw === null) return;
      const mirror = JSON.parse(raw);
      if (mirror?.version !== 1 || !Object.entries(this.identity).every(
        ([key, value]) => mirror.identity?.[key] === value)) return;
      if (!isMaterial(mirror.material) || !isMaterial(mirror.savedSnapshot)
        || !isRevision(mirror.baseRevision)) throw new Error("Invalid session mirror");
      const pending = mirror.pendingSave;
      if (pending && (pending.workspace_id !== this.workspaceId
        || typeof pending.request_id !== "string" || !pending.request_id
        || !isRevision(pending.expected_revision)
        || !isMaterial(pending.material)
        || !same(Object.keys(pending).sort(),
          ["expected_revision", "material", "request_id", "workspace_id"]))) {
        throw new Error("Invalid pending session request");
      }
      this.recoveryChoice = mirror;
    } catch {
      this.mirrorError = "浏览器暂存恢复不可用，当前输入仅保留在内存中。";
    }
  }

  _writeMirror() {
    // Preserve the recoverable copy until an explicit restore/discard decision.
    if (this.recoveryChoice) return;
    try {
      if (!this.storage) throw new Error("Storage unavailable");
      if (!this.dirty && !this.pendingSave) {
        this.storage.removeItem(this.mirrorKey);
      } else {
        // An allowlist intentionally excludes validation, preview, associations,
        // reports and any other transient UI properties.
        const raw = JSON.stringify({
          version: 1, identity: this.identity, material: this.material,
          savedSnapshot: this.savedSnapshot, baseRevision: this._baselineRevision,
          entryKeys: this.entryKeys, pendingSave: this.pendingSave,
        });
        if (Number.isFinite(this.maxMirrorBytes)
          && new TextEncoder().encode(raw).length > this.maxMirrorBytes) {
          throw new Error("Session mirror too large");
        }
        this.storage.setItem(this.mirrorKey, raw);
      }
      this.mirrorError = null;
    } catch {
      // An older mirror left behind would later be offered as if it were current.
      let removed = false;
      try { this.storage?.removeItem(this.mirrorKey); removed = !!this.storage; } catch {}
      this.mirrorError = removed
        ? "浏览器暂存保存失败，完整输入仍保留在内存中。"
        : "浏览器暂存保存失败，且旧暂存可能仍在；完整输入仍保留在内存中。";
    }
  }
}
