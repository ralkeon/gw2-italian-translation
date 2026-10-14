// Runs the statements of /sync on an in-memory SQLite: node test.mjs
import { DatabaseSync } from "node:sqlite";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
const sql = (name) => readFileSync(new URL(`./sql/${name}.sql`, import.meta.url), "utf8");
const [SQL_HISTORY, SQL_UPSERT, SQL_PULL] = ["history", "upsert", "pull"].map(sql);

const db = new DatabaseSync(":memory:");
db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));

let clock = 1000;
function push(author, rows) {
  const json = JSON.stringify(rows);
  clock += 1;
  db.prepare(SQL_HISTORY).run(json, author, clock);
  db.prepare(SQL_UPSERT).run(json, author, clock, 0);
}
// What changed since the last pull, as a client sees it.
let since = 0;
function pull() {
  const rows = db.prepare(SQL_PULL).all(since, 100);
  for (const r of rows) {
    assert.ok(r.seq > since, "seq goes up");
    since = r.seq;
  }
  return rows.map((r) => [r.id, r.it, r.status, r.author]);
}
const H = "00000000000000aa";

push("Ana", [[1, H, "Sword", "Spada", "auto"], [2, H, "Shield", "Scudo", "auto"]]);
assert.deepEqual(pull(), [[1, "Spada", "auto", "Ana"], [2, "Scudo", "auto", "Ana"]]);

// Another machine translation of a known text: the first one stays, nothing changes.
push("Bea", [[1, H, "Sword", "Lama", "auto"], [3, H, "Bow", "Arco", "auto"]]);
assert.deepEqual(pull(), [[3, "Arco", "auto", "Bea"]]);

// A hand correction replaces it, the old text goes to history; the same again changes nothing.
push("Bea", [[1, H, "Sword", "Spadone", "manual"]]);
push("Bea", [[1, H, "Sword", "Spadone", "manual"]]);
assert.deepEqual(pull(), [[1, "Spadone", "manual", "Bea"]]);
// A machine translation never replaces a hand correction.
push("Ana", [[1, H, "Sword", "Spada", "auto"]]);
assert.deepEqual(pull(), []);
// Last hand correction wins.
push("Ana", [[1, H, "Sword", "Spada lunga", "manual"], [2, H, "Shield", "Scudo grande", "manual"]]);
assert.deepEqual(pull(), [[1, "Spada lunga", "manual", "Ana"], [2, "Scudo grande", "manual", "Ana"]]);
assert.deepEqual(
  db.prepare("SELECT id, it, status, author, replaced_by FROM history ORDER BY rowid").all().map(Object.values),
  [[1, "Spada", "auto", "Ana", "Bea"], [1, "Spadone", "manual", "Bea", "Ana"], [2, "Scudo", "auto", "Ana", "Ana"]],
);
// Same text twice in one push: one row, the last text.
push("Bea", [[3, H, "Bow", "Arco lungo", "manual"], [3, H, "Bow", "Arco corto", "manual"]]);
assert.deepEqual(pull(), [[3, "Arco corto", "manual", "Bea"]]);
// A new client gets everything once.
since = 0;
assert.equal(pull().length, 3);

// The spare server hands out seq from its floor up.
const FLOOR = 1e12;
db.prepare(SQL_UPSERT).run(JSON.stringify([[5, H, "Axe", "Ascia", "auto"]]), "Ana", 2000, FLOOR);
assert.equal(db.prepare("SELECT seq FROM translations WHERE id = 5").get().seq, FLOOR + 1);
db.prepare(SQL_UPSERT).run(JSON.stringify([[6, H, "Mace", "Mazza", "auto"]]), "Ana", 2001, FLOOR);
assert.equal(db.prepare("SELECT seq FROM translations WHERE id = 6").get().seq, FLOOR + 2);
// A server merging another's rows keeps their authors.
const merged = JSON.stringify([[5, H, "Axe", "Scure", "manual", "Bea"]]);
db.prepare(SQL_HISTORY).run(merged, "replica", 3000);
db.prepare(SQL_UPSERT).run(merged, "replica", 3000, 0);
assert.deepEqual(Object.values(db.prepare("SELECT it, author FROM translations WHERE id = 5").get()), ["Scure", "Bea"]);
assert.deepEqual(Object.values(db.prepare("SELECT author, replaced_by FROM history ORDER BY rowid DESC LIMIT 1").get()), ["Ana", "Bea"]);
// Trust: a machine translation made by the server (1) replaces one sent by an addon (0), not the
// other way round, and not another of its own; the owner's hand correction (3) beats a
// contributor's (2) whatever the order, and a contributor's never replaces the owner's.
// Row: [id, hash, en, it, status, author, trust].
const text = (id) => db.prepare("SELECT it, status, trust FROM translations WHERE id = ?").get(id);
push("Ana", [[10, H, "Door", "Porta zero", "auto", null, 0]]);
push("Bea", [[10, H, "Door", "Porta server", "auto", null, 1]]);
assert.deepEqual(Object.values(text(10)), ["Porta server", "auto", 1]);
push("Ana", [[10, H, "Door", "Porta altra", "auto", null, 0], [10, H, "Door", "Porta altra", "auto", null, 1]]);
assert.deepEqual(Object.values(text(10)), ["Porta server", "auto", 1]);
push("Bea", [[10, H, "Door", "Porta mano", "manual", null, 2]]);
push("Ana", [[10, H, "Door", "Porta mia", "manual", null, 3]]);
push("Bea", [[10, H, "Door", "Porta di Bea", "manual", null, 2]]);
assert.deepEqual(Object.values(text(10)), ["Porta mia", "manual", 3]);
push("Ana", [[10, H, "Door", "Porta mia 2", "manual", null, 3]]);
assert.deepEqual(Object.values(text(10)), ["Porta mia 2", "manual", 3]);
push("Bea", [[10, H, "Door", "Porta server 2", "auto", null, 1]]);
assert.deepEqual(Object.values(text(10)), ["Porta mia 2", "manual", 3]);
assert.deepEqual(
  db.prepare("SELECT it, replaced_by FROM history WHERE id = 10 ORDER BY rowid").all().map(Object.values),
  [["Porta zero", "Bea"], ["Porta server", "Bea"], ["Porta mano", "Ana"], ["Porta mia", "Ana"]],
);
console.log("ok");
