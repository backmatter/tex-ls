import { unzipSync } from "fflate";

/** Read the executable without writing archive paths or symlinks to disk. */
export function binaryFromZip(archive: Uint8Array, binaryName: string): Uint8Array {
  const entries = Object.values(unzipSync(archive, {
    filter: (entry) => entry.name.split("/").at(-1) === binaryName,
  }));
  if (entries.length !== 1) {
    throw new Error(`Expected exactly one ${binaryName} in the ZIP, found ${entries.length}`);
  }
  return entries[0];
}
