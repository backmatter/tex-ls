#!/usr/bin/env python3
"""Regression checks for release PR synchronization and packaging invariants."""

import json
from pathlib import Path
import tempfile
import unittest

from sync_release_versions import synchronize


class ReleaseVersions(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        (self.root / "Cargo.toml").write_text('[package]\nversion = "0.2.0"\n')
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "tex-ls"\nversion = "0.2.0"\n')
        (self.root / "CHANGELOG.md").write_text(
            "# Changelog\n\n## [0.2.0](https://example.com) - 2026-09-14\n\n"
            "### Added\n\n- Build integration.\n\n## 0.1.0\n\n- Initial server.\n")
        self.extension = self.root / "editors/vscode"
        self.extension.mkdir(parents=True)
        (self.extension / "package.json").write_text(json.dumps({
            "version": "0.1.1", "texLsServerVersion": "0.1.0", "other": True}))
        (self.extension / "package-lock.json").write_text(json.dumps({
            "version": "0.1.1", "packages": {"": {"version": "0.1.1"},
            "node_modules/kept": {"version": "1.2.3"}}}))
        (self.extension / "CHANGELOG.md").write_text("# Changelog\n\n## 0.1.1\n\n- Legacy extension notes.\n")

    def test_mismatch_is_rejected_and_sync_preserves_history_and_dependencies(self):
        with self.assertRaisesRegex(ValueError, "versions must match"):
            synchronize(self.root)
        synchronize(self.root, write=True, changelog=True)
        self.assertEqual(synchronize(self.root, changelog=True, tag="v0.2.0"), "0.2.0")
        lock = json.loads((self.extension / "package-lock.json").read_text())
        self.assertEqual(lock["packages"]["node_modules/kept"]["version"], "1.2.3")
        notes = (self.extension / "CHANGELOG.md").read_text()
        self.assertIn("- Legacy extension notes.", notes)
        self.assertIn("- Build integration.", notes)
        synchronize(self.root, write=True, changelog=True)
        self.assertEqual((self.extension / "CHANGELOG.md").read_text(), notes)

    def test_tag_and_cargo_lock_mismatches_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "tag"):
            synchronize(self.root, write=True, tag="v0.1.1")
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "tex-ls"\nversion = "0.1.0"\n')
        with self.assertRaisesRegex(ValueError, "Cargo.lock"):
            synchronize(self.root, write=True)

    def test_changed_release_notes_replace_existing_entry(self):
        synchronize(self.root, write=True, changelog=True)
        path = self.root / "CHANGELOG.md"
        path.write_text(path.read_text().replace("Build integration.", "Updated integration."))
        with self.assertRaisesRegex(ValueError, "release notes"):
            synchronize(self.root, changelog=True)
        synchronize(self.root, write=True, changelog=True)
        notes = (self.extension / "CHANGELOG.md").read_text()
        self.assertEqual(notes.count("## [0.2.0]"), 1)
        self.assertIn("Updated integration.", notes)


if __name__ == "__main__":
    unittest.main()
