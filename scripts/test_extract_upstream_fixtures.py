#!/usr/bin/env python3
"""Discriminating tests for `extract_upstream_fixtures` (the golden-fixture producer).

    python3 scripts/test_extract_upstream_fixtures.py

**Who executes this file**: gate ⑫ `scripts-tests` discovers every
`scripts/**/test_*.py` recursively and runs each one (`scripts/gates.sh`, and
`.github/workflows/ci.yml` calls the same gate).  The discovered set is pinned by
identity in `scripts/tests.manifest`, so this file must appear there or the gate
goes red — that is the answer to "an all-green file nobody runs rotts silently"
(here: `t1_6_realm_diff_taxonomy/test_realm_diff_taxonomy.py`, 53 green cases,
zero references, for a whole cycle).

**Why this file must exist at all.**  `extract_upstream_fixtures.py` was 1862
lines with **zero tests**, and it produces `contracts/golden/**` — the 365
fixtures gate ⑨ replays.  If it mis-extracts one fixture's method/path/status,
gate ⑨ replays on wrong input *and stays self-consistent with `report.json`* ⇒
⑨ green, CI green, and the 契约等价率 denominator is silently wrong.  Nothing in
the tree would notice.  This is the same family as `route_parity.py` and
`t1_6_realm_diff_taxonomy`, whose zero-reference verdicts were already fixed.

**No upstream Go tree and no network.**  These tests drive the extractor's pure
functions plus `Extractor.load()` + `extract_file()` over a synthetic Go file
written into a temp dir, so they run in a second under plain `python3`.  The
end-to-end byte-for-byte regeneration of the committed tree (`--check`) is a
*different* piece of evidence and needs the real upstream checkout — see
`docs/37` §302 for the run that was done on this slice.

The invariant every case below pins is the same: **a case that cannot be read
with certainty must become a `skipped` row with a machine-checkable reason, never
a fixture whose fields are guessed.**  A silently plausible fixture is worse than
a missing one: the missing one shows up in `extraction-report.tsv`.
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import extract_upstream_fixtures as ex  # noqa: E402

SCAN = ("server/internal/handler",)
COMMIT = "cafefeedcafefeedcafefeedcafefeedcafefeed"
COMMIT_DATE = "2026-01-01T00:00:00+00:00"

# A synthetic upstream test file.  Every `func Test…` below is one shape the
# extractor must classify; the tests assert on the classification, never on a
# hand-counted number of cases.
GO_SOURCE = '''package handler

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"github.com/louloulin/multica/server/internal/testutil"
)

func TestEmptyBody(t *testing.T) {
	req := httptest.NewRequest("DELETE", "/api/issues/x", nil)
	testutil.Call(t, nil, req).Want(http.StatusNoContent)
}

func TestMissingStatus(t *testing.T) {
	req := httptest.NewRequest("GET", "/api/issues/x", nil)
	testutil.Call(t, nil, req).JSON(nil)
}

func TestTwoMethodsSamePath(t *testing.T) {
	g := httptest.NewRequest("GET", "/api/issues/same", nil)
	testutil.Call(t, nil, g).Want(http.StatusOK)
	p := httptest.NewRequest("POST", "/api/issues/same", nil)
	testutil.Call(t, nil, p).Want(http.StatusCreated)
}

func TestTwoStatuses(t *testing.T) {
	resp := authRequest(t, "GET", "/api/issues/x", nil)
	if resp.StatusCode != http.StatusOK {
		t.Fatal("a")
	}
	if resp.StatusCode != http.StatusForbidden {
		t.Fatal("b")
	}
}

func TestNestedBody(t *testing.T) {
	req := httptest.NewRequest("POST", "/api/issues", strings.NewReader(`{"a":{"b":[1,{"c":"}"}]}}`))
	testutil.Call(t, nil, req).Want(http.StatusCreated)
}

func TestUnicodeBody(t *testing.T) {
	req := httptest.NewRequest("POST", "/api/issues", strings.NewReader(`{"title":"\\u4e2d\\u6587 \\"q\\" tab\\there"}`))
	testutil.Call(t, nil, req).Want(http.StatusCreated)
}

func TestMutatedBody(t *testing.T) {
	body := map[string]any{"title": "x"}
	req := httptest.NewRequest("POST", "/api/issues", testutil.JSONBody(body))
	body["title"] = "y"
	testutil.Call(t, nil, req).Want(http.StatusCreated)
}

func TestBracesInComment(t *testing.T) {
	// a stray } brace and a ) paren must not close anything
	req := httptest.NewRequest("GET", "/api/issues/x", nil)
	testutil.Call(t, nil, req).Want(http.StatusOK)
}
'''


class Extracted:
    """Result of one synthetic-tree run, indexed the way a reader would."""

    def __init__(self, extractor: "ex.Extractor"):
        self.ex = extractor
        self.by_test: dict[str, ex.Fixture] = {}
        for f in extractor.fixtures:
            self.by_test[f.source["test"]] = f

    def fixture(self, test: str) -> ex.Fixture:
        self.assert_one(test)
        return self.by_test[test]

    def assert_one(self, test: str) -> None:
        hits = [f for f in self.ex.fixtures if f.source["test"] == test]
        assert len(hits) == 1, "expected exactly 1 fixture for %s, got %d" % (test, len(hits))

    def skips(self, test: str) -> list[ex.Skip]:
        return [s for s in self.ex.skips if s.test == test]

    def assert_skipped(self, test: str, reason: str) -> ex.Skip:
        rows = self.skips(test)
        assert rows, "expected %s to be skipped, but it produced a fixture" % test
        assert reason in [s.reason for s in rows], (
            "%s skipped as %s, expected %s" % (test, [s.reason for s in rows], reason)
        )
        return rows[0]


def run_extractor(go_source: str = GO_SOURCE, filename: str = "probe_test.go") -> Extracted:
    """Extract from a one-file synthetic upstream tree, in a temp dir.

    `extract_file` is called directly instead of `Extractor.run()` because rule
    I4 (`extract_i4_direct_handler.run`) cross-checks the checkout's
    `server/cmd/server/router.go` against the committed route table and hard-exits
    when they disagree — a rule about the *real* upstream tree, not about
    extracting from Go source, and it would make every case here a fixture of
    `docs/fixtures/upstream-routes.tsv` instead of a test of the extractor.
    """
    root = tempfile.mkdtemp(prefix="probe-upstream-")
    os.makedirs(os.path.join(root, SCAN[0]))
    with open(os.path.join(root, SCAN[0], filename), "w", encoding="utf-8") as fh:
        fh.write(go_source)
    extractor = ex.Extractor(root, SCAN, COMMIT, COMMIT_DATE)
    extractor.load()
    for rel in extractor.test_files:
        extractor.extract_file(rel)
    return Extracted(extractor)


class GoldenShape(unittest.TestCase):
    """Shapes upstream actually writes, end to end through the extractor."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.got = run_extractor()

    def test_absent_body_is_not_a_guessed_body(self):
        """`nil` body ⇒ `body: null`, distinct from "body I could not read"."""
        f = self.got.fixture("TestEmptyBody")
        self.assertIsNone(f.body)
        self.assertEqual(f.method, "DELETE")
        self.assertEqual(f.status, 204)
        self.assertEqual(f.path, "/api/issues/x")

    def test_missing_status_assertion_is_skipped_not_invented(self):
        """No `.Want(...)` ⇒ no fixture.  A guessed 200 would prove nothing."""
        self.got.assert_skipped("TestMissingStatus", "no_status_assertion")
        self.assertNotIn("TestMissingStatus", self.got.by_test)

    def test_two_statuses_on_one_response_is_ambiguous(self):
        """Two different assertions on `resp` ⇒ ambiguous, not "the last one"."""
        self.got.assert_skipped("TestTwoStatuses", "ambiguous_status")

    def test_same_path_two_methods_stay_two_fixtures(self):
        """Same path, two methods ⇒ two distinct fixtures, neither overwritten.

        Collapsing them would replay one request twice and lose a route.  Only
        `TestTwoMethodsSamePath` uses `/api/issues/same`, so the count is exact.
        """
        got = [f for f in self.got.ex.fixtures if f.path == "/api/issues/same"]
        self.assertEqual(sorted((f.method, f.status) for f in got), [("GET", 200), ("POST", 201)])

    def test_nested_body_with_braces_inside_strings_decodes(self):
        """Multi-level nesting, including a `}` inside a JSON string literal."""
        f = self.got.fixture("TestNestedBody")
        self.assertEqual(f.body, {"a": {"b": [1, {"c": "}"}]}})

    def test_escapes_in_a_json_string_body_are_decoded_once(self):
        """`\\u4e2d` must arrive as 中文, and the escaped `\"` must not end the string."""
        f = self.got.fixture("TestUnicodeBody")
        self.assertEqual(f.body, {"title": '中文 "q" tab\there'})

    def test_body_mutated_after_construction_yields_no_fixture(self):
        """The request was built from `body` *before* `body` changed ⇒ unverifiable.

        Replaying it would assert a request upstream never sent.
        """
        self.assertNotIn("TestMutatedBody", self.got.by_test)
        self.assertTrue(self.got.skips("TestMutatedBody"))

    def test_braces_in_comments_do_not_close_the_call(self):
        """A `}` and a `)` inside a line comment must not truncate the chain."""
        f = self.got.fixture("TestBracesInComment")
        self.assertEqual((f.method, f.status), ("GET", 200))

    def test_every_candidate_site_is_either_a_fixture_or_a_row(self):
        """Nothing is dropped silently: candidates == produced + skipped."""
        e = self.got.ex
        accounted = sum(e.produced.values()) + len(
            [s for s in e.skips if s.reason != "helper_not_followed" or s.detail != "no recorded outcome"]
        )
        self.assertEqual(accounted, len(e.candidates))
        # and every candidate site has an outcome recorded under its own key
        self.assertEqual(len(e.produced), len(e.candidates))

    def test_internal_error_does_not_hide_the_site(self):
        """A crash on one site is recorded as `internal_error`, never swallowed.

        `extract_file` wraps each site in `try/except Exception`: one malformed
        case must not abort the file, and must not leave the site unaccounted
        for either (that is the "nothing is dropped silently" contract).
        """
        import unittest.mock as mock

        def explode(*_a, **_k):
            raise RuntimeError("probe: injected extractor crash")

        # `map_subset` is reached only from `extract_site`, so this exercises the
        # per-site guard itself rather than the (unguarded) statement walker.
        with mock.patch.object(ex, "map_subset", explode):
            got = run_extractor()
        self.assertTrue(any(s.reason == "internal_error" for s in got.ex.skips))
        self.assertEqual(len(got.ex.produced), len(got.ex.candidates))
        self.assertEqual(len(got.ex.fixtures), 0)
        # and with the crash removed the very same source produces fixtures
        self.assertGreater(len(run_extractor().ex.fixtures), 0)


class GoTextPrimitives(unittest.TestCase):
    """The masking / matching layer — offsets are load-bearing for every caller."""

    def test_mask_preserves_offsets_and_decodes_literals(self):
        src = 'x := "hi" // a } comment\ny := 1\n'
        masked, literals = ex.mask_go(src)
        self.assertEqual(len(masked), len(src))
        self.assertNotIn("comment", masked)
        self.assertIn("// a } comment\n", src)  # the original is untouched
        self.assertEqual(literals[src.index('"')], "hi")

    def test_mask_keeps_the_quote_delimiters(self):
        src = 'f("a}b")\n'
        masked, literals = ex.mask_go(src)
        self.assertIn('"', masked)
        self.assertEqual(literals[src.index('"')], "a}b")

    def test_mask_handles_a_block_comment_with_braces(self):
        src = 'a := 1\n/* } ) { */\nb := 2\n'
        masked, _ = ex.mask_go(src)
        self.assertEqual(len(masked), len(src))
        self.assertNotIn("}", masked.splitlines()[1])
        self.assertEqual(ex.line_of(src, masked.index("b :=")), 3)

    def test_go_unescape_covers_the_escapes_upstream_uses(self):
        self.assertEqual(ex.go_unescape(r"a\nb"), "a\nb")
        self.assertEqual(ex.go_unescape(r"tab\there"), "tab\there")
        self.assertEqual(ex.go_unescape(r"\x41"), "A")
        self.assertEqual(ex.go_unescape(r"\u4e2d"), "中")
        self.assertEqual(ex.go_unescape(r"q\"q"), 'q"q')
        self.assertEqual(ex.go_unescape(r"path\to/file"), "path\to/file")  # \t decodes
        self.assertEqual(ex.go_unescape(r"a\qb"), r"a\qb")  # unknown escape passes through

    def test_matching_finds_the_matching_bracket_across_nesting(self):
        """`matching` is handed the index of an **open** bracket, not a callee."""
        for src in ('f(g(1), map[string]any{"a": {"b": 2}})', 'f(g(1), "}")', "f(g(1))"):
            masked, _ = ex.mask_go(src)
            self.assertEqual(ex.matching(masked, masked.index("(")), len(masked), src)
        # the inner `g(1)` closes on its own `)`, not on the end of the outer call
        masked, _ = ex.mask_go("f(g(1), h(2))")
        self.assertEqual(ex.matching(masked, masked.index("g(") + 1), masked.index("1)") + 2)

    def test_split_args_only_splits_at_depth_zero(self):
        """A `,` inside a nested composite or a string is not an argument break."""
        masked, _ = ex.mask_go('Call(t, h, req, map[string]string{"a": "1,2"})')
        open_paren = masked.index("(")
        args = ex.split_args(masked, open_paren + 1, ex.matching(masked, open_paren) - 1)
        self.assertEqual(len(args), 4)
        self.assertEqual(masked[args[2][0] : args[2][1]].strip(), "req")
        self.assertTrue(masked[args[3][0] : args[3][1]].strip().startswith("map["))

    def test_asi_end_keeps_a_multiline_call_chain_in_one_statement(self):
        masked, _ = ex.mask_go("f(a,\n  b)\ng := 1\n")
        start = masked.index("f(")
        self.assertEqual(masked[start : ex.asi_end(masked, start)].strip(), "f(a,\n  b)")

    def test_statuses_in_reads_both_named_and_bare_statuses(self):
        self.assertEqual(ex.statuses_in("http.StatusNoContent"), [204])
        self.assertEqual(ex.statuses_in("!= 403"), [403])
        self.assertEqual(ex.statuses_in("Wants 1000 items"), [])  # 4 digits is not a status
        self.assertEqual(ex.statuses_in("offset=200"), [200])  # a query value still reads as a status
        self.assertEqual(ex.statuses_in("StatusNope"), [])


class Canonicalisation(unittest.TestCase):
    """`canonicalise` decides the fixture's URL — the replay's actual input."""

    @staticmethod
    def state(pieces, markers=()):
        return ex.ReqState(
            pieces=list(pieces),
            markers={m: ex.Value("symbol", n, "") for m, n in markers},
        )

    def test_a_marker_is_named_after_its_url_param(self):
        """The url-param name wins over the symbol name.

        They are usually the same string upstream, which is why the two rules
        have to be told apart by a case where they differ — rule I4 renames
        `{testWorkspaceID}` to the registered template `{id}`, and a fallback to
        the symbol name would silently un-mount that route.
        """
        mark = ex.MARK + "0" + ex.MARK
        sym = ex.Value("symbol", "$testWorkspaceID", "")
        st = ex.ReqState(
            pieces=["/api/workspaces/", mark],
            markers={mark: sym},
            urlparams={"id": sym},
        )
        path, params, _q = ex.canonicalise(st)
        self.assertEqual(path, "/api/workspaces/{id}")
        self.assertEqual(params, {"id": "$testWorkspaceID"})

    def test_a_marker_with_no_url_param_falls_back_to_its_symbol_name(self):
        st = self.state(["/api/issues/", ex.MARK + "0" + ex.MARK], [(ex.MARK + "0" + ex.MARK, "$testIssueID")])
        path, params, _q = ex.canonicalise(st)
        self.assertEqual(path, "/api/issues/{testIssueID}")
        self.assertEqual(params, {"testIssueID": "$testIssueID"})

    def test_query_markers_are_substituted_and_kept_out_of_the_path(self):
        st = self.state(["/api/issues?workspace_id=", ex.MARK + "0" + ex.MARK],
                        [(ex.MARK + "0" + ex.MARK, "$testWorkspaceID")])
        path, _params, query = ex.canonicalise(st)
        self.assertEqual(path, "/api/issues")
        self.assertEqual(query, {"workspace_id": "$testWorkspaceID"})

    def test_duplicate_slashes_collapse_and_the_trailing_slash_is_dropped(self):
        path, _p, _q = ex.canonicalise(self.state(["/api//issues/"]))
        self.assertEqual(path, "/api/issues")

    def test_the_root_path_survives_the_rstrip(self):
        path, _p, _q = ex.canonicalise(self.state(["/"]))
        self.assertEqual(path, "/")

    def test_an_unsubstituted_marker_never_reaches_a_fixture(self):
        """`extract_site` gates on `MARK in path`; a name is always substituted."""
        st = self.state(["/api/issues/", ex.MARK + "0" + ex.MARK], [(ex.MARK + "0" + ex.MARK, "$testIssueID")])
        path, _p, _q = ex.canonicalise(st)
        self.assertNotIn(ex.MARK, path)
        q = self.state(["/api/issues?x=", ex.MARK + "0" + ex.MARK], [(ex.MARK + "0" + ex.MARK, "$testWorkspaceID")])
        _p2, _pp, query = ex.canonicalise(q)
        self.assertEqual(query["x"], "$testWorkspaceID")
        self.assertNotIn(ex.MARK, query["x"])


class DomainBuckets(unittest.TestCase):
    def test_one_api_prefix_is_stripped_to_the_resource_segment(self):
        """Upstream routes are `/api/<resource>`; the strip is a **single** step.

        Pinned as one step on purpose: `/api/v1/...` buckets as `v1`, so if a
        two-step version were ever introduced it would move fixtures between
        buckets (a `--check` diff of the whole tree) rather than silently pass.
        """
        self.assertEqual(ex.domain_of("/api/issues/1"), "issues")
        self.assertEqual(ex.domain_of("/internal/agents"), "agents")
        self.assertEqual(ex.domain_of("/api/v1/issues/1"), "v1")

    def test_a_leading_path_param_does_not_become_the_bucket(self):
        self.assertEqual(ex.domain_of("/{tenant}/issues"), "issues")

    def test_the_root_is_its_own_bucket(self):
        self.assertEqual(ex.domain_of("/"), "_root")

    def test_non_alphanumerics_in_a_bucket_are_normalised(self):
        self.assertEqual(ex.domain_of("/api/issue-labels"), "issue_labels")


class FixtureIdentity(unittest.TestCase):
    """Ids and slugs are filenames — a collision silently loses a fixture."""

    def make(self, test: str, line: int, domain: str = "issues") -> ex.Fixture:
        return ex.Fixture(
            id="", domain=domain, slug="", method="GET", path="/api/issues/1",
            path_params={}, query={}, headers={}, actor={"kind": "anonymous"}, body=None,
            status=200, json_subset={},
            source={"file": "f_test.go", "line": line, "test": test, "site": "testutil.Call", "via": "router"},
            notes=[], via="router",
        )

    def test_two_fixtures_on_one_line_get_distinct_slugs(self):
        """Same test, same line (a loop table unrolled) ⇒ the ordinal must differ."""
        fx = [self.make("TestLoop", 40), self.make("TestLoop", 40)]
        ex.number_fixtures(fx)
        slugs = [f.slug for f in fx]
        self.assertEqual(len(set(slugs)), 2, "slug collision would overwrite a fixture: %r" % slugs)
        self.assertEqual([f.slug[:3] for f in fx], ["001", "002"])

    def test_ids_are_unique_and_carry_the_ordinal(self):
        fx = [self.make("TestLoop", 40), self.make("TestLoop", 40)]
        ex.number_fixtures(fx)
        ids = [f.id for f in fx]
        self.assertEqual(len(set(ids)), 2)
        self.assertTrue(ids[0].endswith("#1") and ids[1].endswith("#2"))

    def test_numbering_is_reproducible_regardless_of_input_order(self):
        a = [self.make("TestB", 10), self.make("TestA", 20)]
        b = [self.make("TestA", 20), self.make("TestB", 10)]
        ex.number_fixtures(a)
        ex.number_fixtures(b)
        self.assertEqual([f.id for f in a], [f.id for f in b])

    def test_the_ordinal_is_per_domain_not_global(self):
        fx = [self.make("TestA", 10, "issues"), self.make("TestB", 20, "agents")]
        ex.number_fixtures(fx)
        self.assertEqual(sorted(f.slug[:3] for f in fx), ["001", "001"])

    def test_slugify_collapses_runs_and_never_returns_empty(self):
        self.assertEqual(ex.slugify("Test_Get/Thing!!"), "Test-Get-Thing")
        self.assertEqual(ex.slugify("///"), "case")


class EmittedTree(unittest.TestCase):
    """`emit` writes the tree `--check` diffs — its shape *is* the gate's input."""

    def setUp(self) -> None:
        self.got = run_extractor()
        self.tmp = tempfile.mkdtemp(prefix="probe-out-")
        out = os.path.join(self.tmp, "golden")
        os.makedirs(out)
        ex.emit(self.got.ex, out, COMMIT, COMMIT_DATE, SCAN)
        self.out = out

    def read_stats(self) -> dict:
        with open(os.path.join(self.out, "stats.json"), encoding="utf-8") as fh:
            return json.load(fh)

    def read_report(self) -> list[list[str]]:
        with open(os.path.join(self.out, "extraction-report.tsv"), encoding="utf-8") as fh:
            rows = [ln.rstrip("\n").split("\t") for ln in fh]
        self.assertEqual(rows[0], ["file", "line", "test", "site", "outcome", "reason", "detail"])
        return rows[1:]

    def test_stats_by_reason_is_recomputed_from_the_report_rows(self):
        """The two must never disagree: the report is the single record.

        `emit` deliberately overwrites `skipped.by_reason` from the rows — if
        that line is removed, the histogram would drift from the report while
        both files still look well-formed.
        """
        rows = self.read_report()
        counted: dict[str, int] = {}
        for r in rows:
            if r[4] == "skipped":
                counted[r[5]] = counted.get(r[5], 0) + 1
        stats = self.read_stats()
        self.assertEqual(stats["skipped"]["by_reason"], dict(sorted(counted.items())))
        self.assertEqual(stats["skipped"]["total"], len([r for r in rows if r[4] == "skipped"]))

    def test_every_row_has_all_seven_columns_and_a_known_outcome(self):
        for r in self.read_report():
            self.assertEqual(len(r), 7, "ragged row: %r" % r)
            self.assertIn(r[4], ("extracted", "skipped", "helper_site"))
            if r[4] == "skipped":
                self.assertIn(r[5], ex.SKIP_REASONS, "unknown skip reason %r" % r[5])

    def test_the_extraction_rate_denominator_is_the_candidate_count(self):
        """抽取率 must be recomputable by counting files, not by trusting prose."""
        stats = self.read_stats()
        rows = self.read_report()
        site_rows = [r for r in rows if r[3] != "helper_site"]
        self.assertEqual(stats["extraction_rate"]["candidate_sites"], len(site_rows))
        self.assertEqual(
            stats["extraction_rate"]["extracted_sites"], len([r for r in site_rows if r[4] == "extracted"])
        )
        expected = round(len([r for r in site_rows if r[4] == "extracted"]) / len(site_rows), 4)
        self.assertEqual(stats["extraction_rate"]["rate"], expected)

    def test_one_fixture_file_per_fixture_and_nothing_else(self):
        on_disk = set()
        for dirpath, _d, files in os.walk(self.out):
            for fn in files:
                rel = os.path.relpath(os.path.join(dirpath, fn), self.out)
                if rel.endswith(".json") and "/" in rel:
                    on_disk.add(rel)
        self.assertEqual(len(on_disk), len(self.got.ex.fixtures))

    def test_the_pin_records_the_upstream_revision_and_the_scan(self):
        with open(os.path.join(self.out, "PIN"), encoding="utf-8") as fh:
            pin = fh.read()
        self.assertIn("upstream_commit %s" % COMMIT, pin)
        self.assertIn(" ".join(SCAN), pin)
        self.assertIn("schema_version %d" % ex.SCHEMA_VERSION, pin)

    def test_fixture_json_omits_an_empty_requires_block(self):
        """`--check` diffs these files byte-for-byte; an `[]` here would move 365 files."""
        for f in self.got.ex.fixtures:
            block = ex.fixture_json(f, COMMIT)["extraction"]
            if f.requires:
                self.assertIn("requires", block)
            else:
                self.assertNotIn("requires", block)

    def test_fixture_json_round_trips_through_json(self):
        for f in self.got.ex.fixtures:
            blob = json.dumps(ex.fixture_json(f, COMMIT), ensure_ascii=False)
            self.assertEqual(json.loads(blob)["expect"]["status"], f.status)


class TreeDiff(unittest.TestCase):
    def test_diff_trees_names_all_three_kinds_of_drift(self):
        want = {"a.json": b"1", "gone.json": b"2"}
        got = {"a.json": b"9", "new.json": b"3"}
        self.assertEqual(
            ex.diff_trees(want, got),
            ["differs: a.json", "missing: gone.json", "unexpected: new.json"],
        )

    def test_identical_trees_have_no_problems(self):
        self.assertEqual(ex.diff_trees({"a": b"1"}, {"a": b"1"}), [])

    def test_a_one_byte_difference_is_still_a_difference(self):
        self.assertEqual(len(ex.diff_trees({"a": b"1\n"}, {"a": b"1"})), 1)


class CommandLine(unittest.TestCase):
    """`main()` is the only entry point a human or CI ever uses."""

    def test_help_exits_zero(self):
        """argparse exits via SystemExit(0) — the documented rc=0 path for `--help`."""
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            with self.assertRaises(SystemExit) as caught:
                ex.main(["--help"])
        self.assertEqual(caught.exception.code, 0)
        self.assertIn("--upstream", buf.getvalue())
        self.assertIn("--check", buf.getvalue())

    def test_a_missing_upstream_tree_is_a_usage_error_not_a_crash(self):
        """rc=2 must be distinguishable from rc=0 ("ok") and rc=1 ("drift")."""
        err = io.StringIO()
        old = sys.stderr
        sys.stderr = err
        try:
            rc = ex.main(["--upstream", os.path.join(tempfile.gettempdir(), "definitely-not-here-2633")])
        finally:
            sys.stderr = old
        self.assertEqual(rc, 2)
        self.assertIn("is not a directory", err.getvalue())

    def test_a_scan_dir_absent_from_upstream_is_also_a_usage_error(self):
        with tempfile.TemporaryDirectory() as root:
            err = io.StringIO()
            old = sys.stderr
            sys.stderr = err
            try:
                rc = ex.main(["--upstream", root])
            finally:
                sys.stderr = err or old
            self.assertEqual(rc, 2)
            self.assertIn("has no server/internal/handler/", err.getvalue())


class GoldenTreeIsSelfConsistent(unittest.TestCase):
    """The committed tree itself, read (never written) — the fixtures must agree.

    This is the invariant whose violation the whole slice is about: if
    `contracts/golden/**` disagrees with itself, gate ⑨ replays on wrong input
    and still reads green.
    """

    ROOT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "contracts", "golden")

    def fixture_files(self) -> list[str]:
        """`contracts/golden/<bucket>/NNN-….json` only — `stats.json` is not a fixture."""
        out = []
        for dirpath, _d, files in os.walk(self.ROOT):
            for fn in sorted(files):
                full = os.path.join(dirpath, fn)
                if fn.endswith(".json") and os.path.basename(dirpath) != "golden":
                    out.append(full)
        return out

    def fixtures(self) -> list[dict]:
        out = []
        for full in self.fixture_files():
            with open(full, encoding="utf-8") as fh:
                out.append(json.load(fh))
        return out

    def test_fixture_ids_are_unique_across_the_whole_tree(self):
        ids = [f["id"] for f in self.fixtures()]
        self.assertEqual(len(ids), len(set(ids)), "duplicate fixture id ⇒ one file overwrites another")

    def test_every_fixture_file_lives_in_the_bucket_its_domain_names(self):
        for full in self.fixture_files():
            with open(full, encoding="utf-8") as fh:
                blob = json.load(fh)
            self.assertEqual(os.path.basename(os.path.dirname(full)), blob["id"].split("/")[0], full)

    def test_slugs_match_the_ids_they_were_derived_from(self):
        """`<ordinal>-<test>-L<line>`: if the rule drifts, `--check` moves 365 files."""
        import re
        pat = re.compile(r"^\d{3}-.+-L\d+$")
        for full in self.fixture_files():
            self.assertRegex(os.path.basename(full)[: -len(".json")], pat, full)

    def test_statuses_are_real_http_codes(self):
        for f in self.fixtures():
            self.assertIsInstance(f["expect"]["status"], int)
            self.assertTrue(100 <= f["expect"]["status"] <= 599, f["id"])

    def test_methods_are_real_http_methods(self):
        for f in self.fixtures():
            self.assertIn(f["method"], {"GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE"}, f["id"])

    def test_no_fixture_carries_an_unsubstituted_marker(self):
        for f in self.fixtures():
            self.assertNotIn(ex.MARK, f["path"], f["id"])
            for v in f["query"].values():
                self.assertNotIn(ex.MARK, v, f["id"])

    def test_every_binding_a_fixture_names_is_bindable(self):
        """A symbol the runner cannot mint makes the case unevaluable, not wrong."""
        for f in self.fixtures():
            for sym, column in f["extraction"]["bindings"].items():
                self.assertIn(sym, ex.BINDABLE, "%s: %s" % (f["id"], sym))
                self.assertEqual(ex.BINDABLE[sym], column, f["id"])

    def test_every_fixture_records_the_upstream_site_it_came_from(self):
        sites = {k for _p, k in ex.SITE_PATTERNS} | {ex.i4.KIND}
        for f in self.fixtures():
            self.assertIn(f["source"]["site"], sites, f["id"])
            self.assertTrue(f["source"]["commit"], f["id"])


if __name__ == "__main__":
    unittest.main(verbosity=2)