import { deepStrictEqual, throws } from "node:assert/strict";
import { test } from "node:test";
import { zipSync } from "fflate";
import { binaryFromZip } from "../src/zip";

test("reads a nested executable and ignores other archive paths", () => {
  const executable = new Uint8Array([77, 90, 1, 2]);
  const archive = zipSync({
    "release/meaning.exe": executable,
    "../../outside.txt": new Uint8Array([3]),
    "release/README.md": new Uint8Array([4]),
  });
  deepStrictEqual(binaryFromZip(archive, "meaning.exe"), executable);
});

test("rejects missing and ambiguous executables", () => {
  throws(() => binaryFromZip(zipSync({}), "meaning.exe"), /found 0/);
  throws(() => binaryFromZip(zipSync({
    "a/meaning.exe": new Uint8Array([1]),
    "b/meaning.exe": new Uint8Array([2]),
  }), "meaning.exe"), /found 2/);
});
