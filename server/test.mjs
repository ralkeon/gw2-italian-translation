// Runs the statements of /sync on an in-memory SQLite: node test.mjs
import { DatabaseSync } from "node:sqlite";
import { readFileSync } from "node:fs";
import assert from "node:assert/strict";
import { SQL_HISTORY, SQL_UPSERT, SQL_PULL } from "./src/sql.js";

const db = new DatabaseSync(":memory:");
db.exec(readFileSync(new URL("./schema.sql", import.meta.url), "utf8"));

let clock = 1000;
function push(author, rows) {
  const json = JSON.stringify(rows);
  clock += 1;
  db.prepare(SQL_HISTORY).run(json, author, clock);
  db.prepare(SQL_UPSERT).run(json, author, clock);
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
console.log("ok");
