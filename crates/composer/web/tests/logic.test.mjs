import { test } from "node:test";
import assert from "node:assert/strict";
import { parseAddresses, safeLink, fileSize } from "../src/logic.mjs";
test("recipient parsing preserves accented display names and every address", () => {
  assert.deepEqual(
    parseAddresses(
      '"Moreau, Camille" <camille@example.fr>, Léa <lea@example.fr>; work@atelier.example',
    ),
    ["camille@example.fr", "lea@example.fr", "work@atelier.example"],
  );
  assert.deepEqual(parseAddresses(""), []);
  assert.throws(() => parseAddresses("valid@example.fr, invalid"));
});
test("links exclude executable and local file schemes", () => {
  for (const value of [
    "https://example.fr",
    "http://example.fr/a",
    "mailto:camille@example.fr",
  ])
    assert.equal(safeLink(value), true);
  for (const value of [
    "javascript:alert(1)",
    "file:///etc/passwd",
    "data:text/html,test",
    "nonsense",
  ])
    assert.equal(safeLink(value), false);
});
test("attachment sizes use the selected locale", () => {
  assert.match(fileSize(253952, "fr"), /248/);
  assert.match(fileSize(1048576, "en"), /1/);
});
