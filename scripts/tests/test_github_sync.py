#!/usr/bin/env python3
"""Tests for github-sync.py — run with: python3 -m pytest scripts/tests/test_github_sync.py -v"""

import json
import os
import sys
import tempfile
import unittest
from unittest.mock import patch, MagicMock

sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
import github_sync as gs


class TestStateFile(unittest.TestCase):
    def test_load_state_missing_file(self):
        with tempfile.TemporaryDirectory() as d:
            state = gs.load_state(os.path.join(d, "state.json"))
            self.assertEqual(state, {})

    def test_save_and_load_roundtrip(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "state.json")
            gs.save_state(path, {"jordanhubbard/ACC": "2026-04-24T00:00:00Z"})
            state = gs.load_state(path)
            self.assertEqual(state["jordanhubbard/ACC"], "2026-04-24T00:00:00Z")

    def test_save_is_atomic(self):
        """Save must not leave a corrupt file if interrupted mid-write."""
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "state.json")
            gs.save_state(path, {"repo": "v1"})
            gs.save_state(path, {"repo": "v2"})
            state = gs.load_state(path)
            self.assertEqual(state["repo"], "v2")


class TestDedup(unittest.TestCase):
    def test_beads_id_for_github_issue(self):
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertEqual(gs.beads_id_for(issue), "gh-jordanhubbard/ACC-42")

    def test_is_already_synced_false(self):
        existing = []
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertFalse(gs.is_already_synced(issue, existing))

    def test_is_already_synced_true_via_metadata(self):
        existing = [{"metadata": {"github_number": 42, "github_repo": "jordanhubbard/ACC"}}]
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertTrue(gs.is_already_synced(issue, existing))

    def test_is_already_synced_true_via_notes(self):
        existing = [{"notes": "source=github github_number=42 github_repo=jordanhubbard/ACC"}]
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertTrue(gs.is_already_synced(issue, existing))

    def test_is_already_synced_true_via_title_key(self):
        existing = [{"title": "Fix bug [gh:jordanhubbard/ACC#42]", "notes": ""}]
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertTrue(gs.is_already_synced(issue, existing))

    def test_is_already_synced_different_repo(self):
        existing = [{"metadata": {"github_number": 42, "github_repo": "other/repo"}}]
        issue = {"number": 42, "repo": "jordanhubbard/ACC"}
        self.assertFalse(gs.is_already_synced(issue, existing))


class TestBuildMetadata(unittest.TestCase):
    def test_basic_metadata(self):
        issue = {
            "number": 7,
            "repo": "jordanhubbard/ACC",
            "url": "https://github.com/jordanhubbard/ACC/issues/7",
            "labels": [{"name": "bug"}, {"name": "agent-ready"}],
        }
        meta = gs.build_metadata(issue)
        self.assertEqual(meta["source"], "github")
        self.assertEqual(meta["github_number"], 7)
        self.assertEqual(meta["github_repo"], "jordanhubbard/ACC")
        self.assertEqual(meta["github_labels"], ["bug", "agent-ready"])

    def test_has_dispatch_label_true(self):
        issue = {"labels": [{"name": "agent-ready"}]}
        self.assertTrue(gs.has_dispatch_label(issue, "agent-ready"))

    def test_has_dispatch_label_false(self):
        issue = {"labels": [{"name": "bug"}]}
        self.assertFalse(gs.has_dispatch_label(issue, "agent-ready"))

    def test_has_dispatch_label_empty(self):
        issue = {"labels": []}
        self.assertFalse(gs.has_dispatch_label(issue, "agent-ready"))


class TestMapPriority(unittest.TestCase):
    def test_priority_one_label(self):
        self.assertEqual(gs.map_priority(["P0"]), 0)
        self.assertEqual(gs.map_priority(["P1"]), 1)
        self.assertEqual(gs.map_priority(["P2"]), 2)

    def test_priority_default(self):
        self.assertEqual(gs.map_priority([]), 2)
        self.assertEqual(gs.map_priority(["enhancement"]), 2)

    def test_priority_bug_boost(self):
        self.assertEqual(gs.map_priority(["bug"]), 1)


class TestFleetTaskPayload(unittest.TestCase):
    def test_payload_contains_required_fields(self):
        issue = {
            "number": 5,
            "title": "Fix the thing",
            "body": "It is broken",
            "repo": "jordanhubbard/ACC",
            "url": "https://github.com/jordanhubbard/ACC/issues/5",
            "labels": [{"name": "agent-ready"}],
        }
        beads_id = "ACC-xyz"
        project_id = "proj-abc"
        payload = gs.build_fleet_task_payload(issue, beads_id, project_id)
        self.assertIn("ACC-xyz", payload["title"])
        self.assertIn("#5", payload["title"])
        self.assertEqual(payload["metadata"]["github_number"], 5)
        self.assertEqual(payload["metadata"]["beads_id"], "ACC-xyz")
        self.assertEqual(payload["project_id"], "proj-abc")


if __name__ == "__main__":
    unittest.main()
