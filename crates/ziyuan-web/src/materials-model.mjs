// Form state only: no DOM, network client, or temporary preview associations.
const clone = (value) => JSON.parse(JSON.stringify(value));
const same = (left, right) => JSON.stringify(left) === JSON.stringify(right);
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
 */
export class MaterialModel {
  constructor({
    project, origin = globalThis.location?.origin ?? "", module = "materials",
    storage, requestId = defaultRequestId, refresh = () => {},
    maxMirrorBytes = 1024 * 1024,
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
    if (storage === undefined) {
      try { storage = globalThis.sessionStorage; } catch {
        this.mirrorError = "Session storage is unavailable; input remains in memory.";
      }
    }
    this.storage = storage;
  }

  get canSave() { return this.material !== null && this.workspaceId !== null; }
  get conflict() { return this.canSave && this._baselineRevision !== this.remoteRevision; }
  get dirty() {
    return this.canSave && (this.conflict || !same(this.material, this.savedSnapshot));
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

  receive(view) {
    if (typeof view.workspace_id !== "string" || !view.workspace_id
      || view.project?.id !== this.project
      || (view.draft && (view.draft.project !== this.project
        || typeof view.draft.revision !== "string" || !isMaterial(view.draft.material)))) {
      throw new Error("The materials view has missing or mismatched identity.");
    }
    const changedWorkspace = this.workspaceId !== view.workspace_id;
    const keepLocal = !changedWorkspace && this.dirty;
    if (changedWorkspace) {
      this._generation++;
      this._requests.clear();
      this._acknowledged.clear();
      this.pendingSave = null;
      this.recoveryChoice = null;
      this.mirrorError = null;
    }
    this.workspaceId = view.workspace_id;
    this.remoteRevision = view.draft?.revision ?? null;
    this._remoteMaterial = clone(view.draft?.material ?? emptyMaterial());
    this.needsRefresh = false;
    if (!keepLocal) {
      if (changedWorkspace || !same(this.material, this._remoteMaterial)) {
        this.material = clone(this._remoteMaterial);
      }
      this.savedSnapshot = clone(this._remoteMaterial);
      this._baselineRevision = this.remoteRevision;
      this.localEpoch++;
    }
    this.validation = null;
    this.invalidatePreview();
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
    if (this.pendingSave && same(this.pendingSave.material, this.material)) {
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
    if (sent.generation !== this._generation
      || request.expected_revision !== this.remoteRevision) {
      this.needsRefresh = true;
      this._writeMirror();
      this.refresh();
      return false;
    }
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
      this.mirrorError = "Session recovery is unavailable; input remains in memory.";
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
        if (new TextEncoder().encode(raw).length > this.maxMirrorBytes) {
          throw new Error("Session mirror too large");
        }
        this.storage.setItem(this.mirrorKey, raw);
      }
      this.mirrorError = null;
    } catch {
      this.mirrorError = "Session mirror could not be saved; complete input remains in memory.";
    }
  }
}
