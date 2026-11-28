"""Writes docs/CODEMAP.md: where each feature lives in the crates and which Telegram Desktop file it ports.

    python -I -X utf8 tools/codemap/gen.py           # rewrite docs/CODEMAP.md
    python -I -X utf8 tools/codemap/gen.py --check   # exit 1 when docs/CODEMAP.md is stale

Everything in the map is quoted from the code: the `//!` module docs, the Telegram Desktop files
they (or "port of" comments) name, and the item names. Nothing is guessed: a file without a module
doc is listed under "Undocumented files" with its size. The feature index at the top comes from the
short hand-written tools/codemap/features.toml (feature name -> search terms, symbols, pinned paths).

Sizes and line positions are rounded to two significant figures, so ordinary edits do not make the
map stale; adding, removing or renaming a file, changing a module doc or a public item name does.
Files are the ones git tracks under crates/ (`git add` a new file before regenerating).

Standard library only, deterministic (no dates, no hashes, sorted output), runs in a few seconds.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
import tomllib
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
OUT = ROOT / "docs" / "CODEMAP.md"
FEATURES = HERE / "features.toml"
ARCH = ROOT / "docs" / "ARCHITECTURE.md"

APP_CRATES = (
    "tg-app-core",
    "tg-app-live",
    "tg-app-history",
    "tg-app-info",
    "tg-app-media",
    "tg-app-settings",
    "tg-app-window",
    "tg-app",
)
LARGEST = 30
SUMMARY_LEN = 180
SHORT_LEN = 110
FEATURE_SRC = 8
FEATURE_TESTS = 4
SYMBOL_FILES = 6

# Telegram Desktop source files as the docs name them: `history/history.cpp`,
# `Telegram/SourceFiles/api/api_views.{h,cpp}`, `apiwrap.cpp:1048-1066`, `history_view_html_widget.*`.
TD_RE = re.compile(
    r"(?<![\w./-])((?:Telegram/)?(?:SourceFiles/)?(?:[a-z0-9_]+/)*[a-z0-9_]+)"
    r"\.(\{h, ?cpp\}|\{cpp, ?h\}|cpp|h|mm|\*)(?![\w])"
)
DECL_LINE_RE = re.compile(r"^(?:pub|impl|    pub)[^\n]*", re.M)
ITEM_RE = re.compile(
    r"pub(?:\([^)]*\))?\s+(?:(?:async|const|unsafe|extern \"C\")\s+)*(struct|enum|trait|type|fn)\s+(\w+)"
)
TRAIT_IMPL_RE = re.compile(r"impl(?:<[^>{]*>)?\s+([\w:]+)(?:<[^>{]*>)?\s+for\s+([\w:]+)")
FN_DEF_RE = re.compile(r"\bfn\s+([a-z_][a-z0-9_]*)")
BORING_TRAITS = {"Default", "Drop", "Clone", "Debug", "Display", "From", "PartialEq", "Eq", "Hash", "Deref"}


@dataclass
class SrcFile:
    path: str  # repo-relative, forward slashes
    crate: str
    kind: str  # src | test | example | bench | build
    lines: int
    doc: list[str]
    summary: str
    items: list[str]
    tdesktop: list[str]
    norm_all: str = ""
    norm_path: str = ""
    norm_summary: str = ""
    norm_items: str = ""


@dataclass
class Crate:
    name: str
    files: list[SrcFile] = field(default_factory=list)
    lib_doc: list[str] = field(default_factory=list)
    description: str = ""


# ---------------------------------------------------------------- helpers


def approx(n: int) -> str:
    """Two significant figures: 7 -> "7", 449 -> "450", 3322 -> "3.3k", 14881 -> "15k"."""
    if n < 100:
        return str(n)
    digits = len(str(n))
    r = round(n, 2 - digits)
    if r < 1000:
        return str(r)
    if r < 10000:
        return f"{r / 1000:.1f}k"
    if r < 1_000_000:
        return f"{r // 1000}k"
    return f"{r / 1_000_000:.1f}M"


def approx_pos(n: int) -> str:
    return str(round(n, 2 - len(str(n)))) if n >= 100 else str(n)


def tracked_files() -> list[str]:
    try:
        out = subprocess.run(
            ["git", "-C", str(ROOT), "ls-files", "-z", "--", "crates"],
            capture_output=True,
            check=True,
        ).stdout.decode("utf-8")
        return sorted(p for p in out.split("\0") if p)
    except (OSError, subprocess.CalledProcessError):
        found = []
        for p in (ROOT / "crates").rglob("*"):
            rel = p.relative_to(ROOT).as_posix()
            if p.is_file() and "/target/" not in rel:
                found.append(rel)
        return sorted(found)


def read_text(rel: str) -> str:
    return (ROOT / rel).read_bytes().decode("utf-8", errors="replace").replace("\r\n", "\n")


def count_lines(text: str) -> int:
    if not text:
        return 0
    return text.count("\n") + (0 if text.endswith("\n") else 1)


def module_doc(text: str) -> list[str]:
    """The leading `//!` block (inner attributes, `//` comments and blank lines before it are skipped)."""
    doc: list[str] = []
    for line in text[:20000].split("\n")[:200]:
        s = line.strip()
        if s.startswith("//!"):
            doc.append(s[3:].removeprefix(" ").rstrip())
        elif not doc and (not s or s.startswith("#![") or s.startswith("// ")):
            continue
        else:
            break
    while doc and not doc[-1]:
        doc.pop()
    return doc


def first_paragraph(doc: list[str]) -> str:
    para: list[str] = []
    for line in doc:
        if not line.strip():
            if para:
                break
            continue
        para.append(line.strip())
    return " ".join(para)


def clean_md(s: str) -> str:
    s = re.sub(r"\[(`[^`\]]+`)\]\([^)]*\)", r"\1", s)
    s = re.sub(r"\[(`[^`\]]+`)\]", r"\1", s)
    return s.replace("|", "\\|")


def shorten(s: str, limit: int) -> str:
    s = clean_md(s)
    if len(s) <= limit:
        return s
    cut = s[:limit]
    sp = cut.rfind(" ")
    if sp > limit // 2:
        cut = cut[:sp]
    if cut.count("`") % 2:
        cut = cut[: cut.rfind("`")].rstrip()
    return cut.rstrip(" ,;:(-") + " …"


def td_refs(text: str) -> list[str]:
    """Telegram Desktop files named in `text`, normalized, in order of first mention."""
    seen: dict[str, None] = {}
    for m in TD_RE.finditer(text):
        stem, ext = m.group(1), m.group(2)
        stem = stem.removeprefix("Telegram/").removeprefix("SourceFiles/")
        base = stem.rsplit("/", 1)[-1]
        if ext == "h" and "/" not in stem and "_" not in base:
            continue  # `self.h`, `rect.h`: Rust field access, not a file
        if ext == "*" and "_" not in base:
            continue
        if ext not in ("h", "*", "mm"):
            ext = "cpp"
        seen.setdefault(f"{stem}.{ext}", None)
    refs = list(seen)
    # `x.h` next to `x.cpp`, and a bare `x.cpp` next to `dir/x.cpp`, are one file.
    stems = {r.rsplit(".", 1)[0] for r in refs if r.endswith(".cpp")}
    full_bases = {r.rsplit("/", 1)[-1] for r in refs if "/" in r}
    out = []
    for r in refs:
        stem, ext = r.rsplit(".", 1)
        if ext == "h" and stem in stems:
            continue
        if "/" not in r and r in full_bases:
            continue
        out.append(r)
    return out


def comment_td_refs(text: str) -> list[str]:
    """Telegram Desktop files named in `//` comments anywhere in the file, most mentioned first."""
    counts: Counter[str] = Counter()
    order: dict[str, int] = {}
    for m in re.finditer(r"//[^\n]*\.(?:cpp|h|mm)\b[^\n]*", text):
        for r in td_refs(m.group(0)):
            counts[r] += 1
            order.setdefault(r, len(order))
    return sorted(counts, key=lambda r: (-counts[r], order[r]))


def key_items(text: str) -> list[str]:
    types, fns, impls, methods = [], [], [], []
    for m in DECL_LINE_RE.finditer(text):
        line = m.group(0)
        if line.startswith("impl"):
            t = TRAIT_IMPL_RE.match(line)
            if t:
                trait = t.group(1).rsplit("::", 1)[-1]
                if trait not in BORING_TRAITS:
                    impls.append(f"impl {trait}")
            continue
        it = ITEM_RE.match(line.strip())
        if not it:
            continue
        kind, name = it.group(1), it.group(2)
        if line.startswith("    "):
            if kind == "fn":
                methods.append(name + "()")
        elif kind == "fn":
            fns.append(name + "()")
        else:
            types.append(name)
    picked: list[str] = []
    for group, cap in ((types, 4), (impls, 2), (fns, 3), (methods, 3)):
        for name in dict.fromkeys(group):
            if len(picked) >= 6 or cap == 0:
                break
            picked.append(name)
            cap -= 1
    return picked


def file_kind(rel: str) -> str:
    parts = rel.split("/")
    name = parts[-1]
    if "tests" in parts[2:-1]:
        return "test"
    if parts[2] == "examples":
        return "example"
    if parts[2] == "benches":
        return "bench"
    if name == "build.rs" and len(parts) == 3:
        return "build"
    stem = name[:-3]
    if stem == "tests" or stem.startswith("tests_") or stem.endswith("_tests") or "_tests_" in stem:
        return "test"
    return "src"


def norm(s: str) -> str:
    s = re.sub(r"([a-z0-9])([A-Z])", r"\1 \2", s)
    s = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1 \2", s)
    return " " + re.sub(r"[^a-z0-9]+", " ", s.lower()).strip() + " "


@dataclass
class Repo:
    crates: dict[str, Crate]
    files: list[SrcFile]
    texts: dict[str, str]
    fn_defs: dict[str, list[str]]


def load() -> Repo:
    crates: dict[str, Crate] = {}
    files: list[SrcFile] = []
    texts: dict[str, str] = {}
    fn_defs: dict[str, list[str]] = {}
    for rel in tracked_files():
        parts = rel.split("/")
        if len(parts) < 3:
            continue
        cname = parts[1]
        crate = crates.setdefault(cname, Crate(cname))
        if parts[2] == "Cargo.toml":
            m = re.search(r'^description\s*=\s*"([^"]*)"', read_text(rel), re.M)
            if m:
                crate.description = m.group(1)
            continue
        if not rel.endswith(".rs"):
            continue
        text = read_text(rel)
        texts[rel] = text
        doc = module_doc(text)
        tds = td_refs("\n".join(doc))
        if not tds:
            tds = comment_td_refs(text)[:3]
        f = SrcFile(
            path=rel,
            crate=cname,
            kind=file_kind(rel),
            lines=count_lines(text),
            doc=doc,
            summary=first_paragraph(doc),
            items=key_items(text),
            tdesktop=tds,
        )
        f.norm_path = norm(rel.split("/", 2)[2].removesuffix(".rs"))
        f.norm_summary = norm(f.summary)
        f.norm_items = norm(" ".join(f.items))
        f.norm_all = norm(" ".join(doc))
        files.append(f)
        crate.files.append(f)
        if f.kind == "src":
            for name in dict.fromkeys(FN_DEF_RE.findall(text)):
                fn_defs.setdefault(name, []).append(rel)
        if rel == f"crates/{cname}/src/lib.rs" or (rel == f"crates/{cname}/src/main.rs" and not crate.lib_doc):
            crate.lib_doc = doc
    return Repo(crates, files, texts, fn_defs)


def arch_rows() -> dict[str, tuple[str, str]]:
    rows: dict[str, tuple[str, str]] = {}
    if not ARCH.exists():
        return rows
    for line in ARCH.read_text(encoding="utf-8").split("\n"):
        m = re.match(r"^\|\s*`([\w-]+)`\s*\|(.*)\|(.*)\|\s*$", line)
        if m:
            rows[m.group(1)] = (m.group(2).strip(), m.group(3).strip())
    return rows


# ---------------------------------------------------------------- Rust block scanner (split points)

TOKEN_RE = re.compile(r'//|/\*|b?r#*"|"|\'|[{};]')
RAW_PREFIX_RE = re.compile(r'b?r(#*)"')


def blocks(text: str, want_depth: int) -> list[tuple[int, int, str]]:
    """Items whose body opens at brace depth `want_depth`: (first line, last line, header text).

    A small lexer: comments (nested block comments too), strings, raw strings and char literals
    are skipped, so braces inside them do not count.
    """
    n = len(text)
    pos, depth = 0, 0
    item_from = 0  # where the text of the next item at `want_depth` starts
    out: list[tuple[int, int, str]] = []
    stack: list[tuple[int, int]] = []  # (header start, brace position)
    while True:
        m = TOKEN_RE.search(text, pos)
        if not m:
            break
        tok, i = m.group(0), m.start()
        if tok == "//":
            j = text.find("\n", i)
            pos = n if j < 0 else j
        elif tok == "/*":
            nest, j = 1, i + 2
            while nest and j < n:
                a, b = text.find("/*", j), text.find("*/", j)
                if b < 0:
                    j = n
                    break
                if 0 <= a < b:
                    nest, j = nest + 1, a + 2
                else:
                    nest, j = nest - 1, b + 2
            pos = j
        elif tok[0] in "br":
            if i > 0 and (text[i - 1].isalnum() or text[i - 1] == "_"):
                pos = i + 1  # an identifier ending in b/r before a string, e.g. `attr"`: not raw
                continue
            hashes = RAW_PREFIX_RE.match(text, i).group(1)
            j = text.find('"' + hashes, m.end())
            pos = n if j < 0 else j + 1 + len(hashes)
        elif tok == '"':
            j = i + 1
            while j < n:
                c = text[j]
                if c == "\\":
                    j += 2
                elif c == '"':
                    break
                else:
                    j += 1
            pos = j + 1
        elif tok == "'":
            if i + 1 < n and text[i + 1] == "\\":
                j = text.find("'", i + 3)
                pos = n if j < 0 else j + 1
            elif i + 2 < n and text[i + 2] == "'":
                pos = i + 3
            else:
                pos = i + 1  # lifetime
        elif tok == "{":
            if depth == want_depth:
                stack.append((item_from, i))
            depth += 1
            if depth == want_depth:
                item_from = i + 1
            pos = i + 1
        elif tok == "}":
            depth -= 1
            if depth == want_depth and stack:
                hstart, brace = stack.pop()
                header = text[hstart:brace]
                lead = len(header) - len(header.lstrip())
                first = text.count("\n", 0, hstart + lead) + 1
                out.append((first, text.count("\n", 0, i) + 1, header))
                item_from = i + 1
            pos = i + 1
        else:  # ;
            if depth == want_depth:
                item_from = i + 1
            pos = i + 1
    return out


def block_label(header: str) -> str:
    lines = [ln.strip() for ln in header.split("\n")]
    h = " ".join(ln for ln in lines if ln and not ln.startswith("#") and not ln.startswith("//"))
    h = re.sub(r"\s+", " ", h)
    m = re.match(r"(?:pub(?:\([^)]*\))? )?(?:(?:async|const|unsafe) )*fn (\w+)", h)
    if m:
        return f"fn {m.group(1)}"
    if h.startswith("impl"):
        body = re.sub(r"^impl(?:<[^{]*?>)? ?", "", h)
        for _ in range(3):
            body = re.sub(r"<[^<>]*>", "", body)
        body = re.sub(r"\b\w+::", "", body.split(" where ")[0]).strip()
        return f"impl {body}"
    m = re.match(r"(?:pub(?:\([^)]*\))? )?(mod|struct|enum|trait|union|macro_rules!) ?(\w+)", h)
    if m:
        return f"{m.group(1)} {m.group(2)}"
    return shorten(h, 40)


def split_points(text: str, total: int) -> tuple[list[str], int]:
    """Big top-level blocks, and the big methods of a block that holds most of the file."""
    top = blocks(text, 0)
    inner_all: list[tuple[int, int, str]] | None = None
    sized = sorted(top, key=lambda b: (-(b[1] - b[0]), b[0]))
    big = [b for b in sized if b[1] - b[0] + 1 >= max(80, total // 25)][:10]
    big.sort(key=lambda b: b[0])
    parts = []
    for s, e, h in big:
        size = e - s + 1
        entry = f"`{block_label(h)}` ~{approx_pos(s)}-{approx_pos(e)} ({approx(size)})"
        if size * 100 >= total * 40:
            if inner_all is None:
                inner_all = blocks(text, 1)
            inner = [b for b in inner_all if s <= b[0] and b[1] <= e and b[1] - b[0] + 1 >= max(60, size // 30)]
            inner = sorted(inner, key=lambda b: (-(b[1] - b[0]), b[0]))[:6]
            inner.sort(key=lambda b: b[0])
            if inner:
                entry += ", its largest: " + ", ".join(
                    f"`{block_label(h2)}` ({approx(e2 - s2 + 1)})" for s2, e2, h2 in inner
                )
        parts.append(entry)
    mods = len(re.findall(r"^\s*(?:pub(?:\([^)]*\))? )?mod \w+;", text, re.M))
    return parts, mods


# ---------------------------------------------------------------- feature index


def term_re(term: str) -> re.Pattern[str]:
    return re.compile(r" " + re.escape(norm(term).strip()))


def feature_index(repo: Repo) -> list[dict]:
    data = tomllib.loads(FEATURES.read_text(encoding="utf-8"))
    out = []
    by_path = {f.path: f for f in repo.files}
    for feat in data.get("feature", []):
        terms = [term_re(t) for t in feat.get("terms", [])]
        pins = [p for p in feat.get("paths", []) if p in by_path]
        missing = [p for p in feat.get("paths", []) if p not in by_path]
        symbols = []
        sym_bonus: Counter[str] = Counter()
        in_symbol_lines: set[str] = set()
        for sym in feat.get("symbols", []):
            defs = repo.fn_defs.get(sym, [])
            # A call: `name(` or `name::<T>(` not preceded by `fn `.
            call = re.compile(r"(?<!fn )(?<![\w])" + re.escape(sym) + r"\s*(?:::<[^>]*>)?\(")
            users = [
                f.path
                for f in repo.files
                if f.kind == "src" and sym in repo.texts[f.path] and call.search(repo.texts[f.path])
            ]
            for p in defs + users:
                sym_bonus[p] += 1
                in_symbol_lines.add(p)
            symbols.append((sym, defs, [p for p in users if p not in defs]))
        scored = []
        for f in repo.files:
            score = 0
            for t in terms:
                if t.search(f.norm_path):
                    score += 5
                if t.search(f.norm_summary):
                    score += 3
                elif t.search(f.norm_all):
                    score += 1
                if t.search(f.norm_items):
                    score += 1
            # A file the symbol lines already name (the trait, the live session, the mocks) is
            # listed again only when its own path or doc matches well, so the specific files
            # are not pushed out by the big ones that implement every backend method.
            if f.path in in_symbol_lines and score < 5 and f.path not in pins:
                continue
            score += sym_bonus[f.path]
            if f.path in pins:
                score += 100
            if score >= feat.get("min_score", 3):
                scored.append((-score, not f.crate.startswith("tg-app"), f.path, f))
        scored.sort(key=lambda x: x[:3])
        src = [x[3] for x in scored if x[3].kind == "src"][:FEATURE_SRC]
        tests = [x[3] for x in scored if x[3].kind == "test"][:FEATURE_TESTS]
        tds: Counter[str] = Counter()
        first: dict[str, int] = {}
        for f in src:
            for r in f.tdesktop:
                tds[r] += 1
                first.setdefault(r, len(first))
        out.append(
            {
                "name": feat["name"],
                "aka": feat.get("aka", []),
                "terms": feat.get("terms", []),
                "symbols": symbols,
                "src": src,
                "tests": tests,
                "tdesktop": sorted(tds, key=lambda r: (-tds[r], first[r]))[:5],
                "missing": missing,
            }
        )
    return out


# ---------------------------------------------------------------- rendering


def td_cell(f: SrcFile, cap: int = 3) -> str:
    if not f.tdesktop:
        return "—"
    refs = [f"`{r}`" for r in f.tdesktop[:cap]]
    if len(f.tdesktop) > cap:
        refs.append(f"+{len(f.tdesktop) - cap}")
    return ", ".join(refs)


def items_cell(f: SrcFile) -> str:
    return ", ".join(f"`{x}`" for x in f.items) or "—"


def summary_of(f: SrcFile, limit: int) -> str:
    return shorten(f.summary, limit) if f.summary else "*(no module doc)*"


def path_list(paths: list[str]) -> str:
    shown = ", ".join(f"`{p}`" for p in paths[:SYMBOL_FILES])
    if len(paths) > SYMBOL_FILES:
        shown += f" (+{len(paths) - SYMBOL_FILES})"
    return shown


def render(repo: Repo) -> str:
    crates, files = repo.crates, repo.files
    out: list[str] = []
    w = out.append
    total_lines = sum(f.lines for f in files)
    undocumented = [f for f in files if not f.doc]
    app_src = [f for f in files if f.crate in APP_CRATES and f.kind == "src"]
    no_td = [f for f in app_src if not f.tdesktop]

    w("# Rustgram code map")
    w("")
    w("<!-- Generated by tools/codemap/gen.py. Do not edit: run `python -I -X utf8 tools/codemap/gen.py`. -->")
    w("")
    w(
        "Where each feature lives and which Telegram Desktop file it ports. Every description below is quoted "
        "from the code: the first paragraph of a file's `//!` module doc, the Telegram Desktop files that doc "
        "(or, failing that, a `//` \"port of\" comment) names, and the file's public item names. Telegram "
        "Desktop paths are relative to `Telegram/SourceFiles/` (`lib_*/` ones to `Telegram/`). Sizes are "
        "lines, rounded to two significant "
        "figures (`3.3k`), so ordinary edits do not make the map stale."
    )
    w("")
    w("How to look something up:")
    w("")
    w("1. Search this file for the feature word, e.g. `grep -n -i \"voice\" docs/CODEMAP.md`. Every row holds")
    w("   the full repo path, so the hit is the answer. Start with the [feature index](#feature-index).")
    w("2. Not there? Search the [file tables](#tg-app-crates-file-by-file) for a Telegram Desktop file name")
    w("   (`history_view_message`) or a type name (`HistoryWidget`).")
    w("3. A missing or wrong description is fixed in the code: edit the file's `//!` doc, then regenerate.")
    w("   A feature missing from the index goes into `tools/codemap/features.toml` (how: see its header).")
    w("")
    w(
        f"Totals: {len(crates)} crates, {len(files)} Rust files, about {approx(total_lines)} lines; "
        f"{len(undocumented)} files without a module doc; {len(no_td)} of {len(app_src)} tg-app* source files "
        "name no Telegram Desktop file."
    )
    w("")
    w(
        "Contents: [Feature index](#feature-index) · [Crates](#crates) · "
        "[tg-app* crates file by file](#tg-app-crates-file-by-file) · "
        "[Other crates file by file](#other-crates-file-by-file) · "
        "[Largest files and split points](#largest-files-and-split-points) · "
        "[Undocumented files](#undocumented-files)"
    )
    w("")

    # Feature index
    w("## Feature index")
    w("")
    w(
        "Generated from `tools/codemap/features.toml`. *Symbols*: where the named functions are defined and "
        "which other source files call them. Then the source files whose path, module doc or item names "
        "match the feature's terms, best match first (path hits count most), and the matching tests. "
        "*tdesktop* lists the Telegram Desktop files those source files name most often."
    )
    w("")
    for feat in feature_index(repo):
        aka = f" (also: {', '.join(feat['aka'])})" if feat["aka"] else ""
        w(f"### {feat['name']}{aka}")
        w("")
        terms = ", ".join(f"`{t}`" for t in feat["terms"])
        td = ", ".join(f"`{r}`" for r in feat["tdesktop"])
        w(f"Terms: {terms}." + (f" tdesktop: {td}." if td else ""))
        w("")
        for sym, defs, users in feat["symbols"]:
            if not defs:
                w(f"- `{sym}()`: **not defined anywhere** (update features.toml)")
                continue
            line = f"- `{sym}()` defined in {path_list(defs)}"
            if users:
                line += f"; called from {path_list(users)}"
            w(line)
        for f in feat["src"]:
            w(f"- `{f.path}` ({approx(f.lines)}): {summary_of(f, SHORT_LEN)}")
        if feat["tests"]:
            w("- tests: " + ", ".join(f"`{f.path}`" for f in feat["tests"]))
        for p in feat["missing"]:
            w(f"- **pinned path no longer exists:** `{p}` (update features.toml)")
        if not feat["src"] and not feat["tests"]:
            w("- *(no file matches the terms: update features.toml)*")
        w("")

    # Crates
    arch = arch_rows()
    w("## Crates")
    w("")
    w(
        "One paragraph per crate: the first paragraph of its `lib.rs` (or `main.rs`) module doc, the crate's "
        "`Cargo.toml` description and its row in docs/ARCHITECTURE.md where they exist."
    )
    w("")
    for name in sorted(crates):
        c = crates[name]
        if not c.files:
            continue
        src = [f for f in c.files if f.kind == "src"]
        tests = [f for f in c.files if f.kind == "test"]
        lines = sum(f.lines for f in c.files)
        w(f"### {name}")
        w("")
        text = first_paragraph(c.lib_doc)
        if text:
            w(clean_md(text))
            w("")
        if c.description and c.description not in text:
            w(f"Cargo.toml: {clean_md(c.description)}")
            w("")
        if name in arch:
            owns, td = arch[name]
            w(f"ARCHITECTURE.md: {owns} (tdesktop: {td})")
            w("")
        dirs: Counter[str] = Counter()
        flat = 0
        for f in src:
            rest = f.path.split("/")[2:]
            if len(rest) > 2:
                dirs["/".join(rest[:2]) + "/"] += 1
            else:
                flat += 1
        dir_list = ", ".join(f"`{d}` ({k})" for d, k in sorted(dirs.items()))
        w(
            f"{len(src)} source files, {len(tests)} test files, about {approx(lines)} lines."
            + (f" Module directories (source files): {dir_list}; outside them: {flat}." if dir_list else "")
        )
        w("")

    # tg-app* tables
    w("## tg-app* crates file by file")
    w("")
    w(
        "One table per directory. *What it is* is the first paragraph of the module doc (cut at "
        f"{SUMMARY_LEN} characters); *key items* are public types, trait impls and functions in file order; "
        "*tdesktop* is what the module doc names (or, failing that, the most named file in `//` comments). "
        "Test modules follow in a shorter list."
    )
    w("")
    for name in APP_CRATES:
        c = crates.get(name)
        if not c:
            continue
        w(f"### {name}")
        w("")
        groups: dict[str, list[SrcFile]] = {}
        for f in c.files:
            if f.kind in ("src", "build"):
                groups.setdefault(f.path.rsplit("/", 1)[0] + "/", []).append(f)
        for d in sorted(groups):
            w(f"#### `{d}`")
            w("")
            w("| file | lines | what it is | key items | tdesktop |")
            w("|---|---:|---|---|---|")
            for f in sorted(groups[d], key=lambda f: f.path):
                w(
                    f"| `{f.path}` | {approx(f.lines)} | {summary_of(f, SUMMARY_LEN)} | {items_cell(f)} | "
                    f"{td_cell(f)} |"
                )
            w("")
        tests = sorted((f for f in c.files if f.kind not in ("src", "build")), key=lambda f: f.path)
        if tests:
            w(f"#### {name} tests and examples")
            w("")
            for f in tests:
                w(f"- `{f.path}` ({approx(f.lines)}): {summary_of(f, SHORT_LEN)}")
            w("")

    # other crates
    w("## Other crates file by file")
    w("")
    w("One line per file: lines, the first paragraph of the module doc (shortened), the tdesktop files it names.")
    w("")
    for name in sorted(crates):
        if name in APP_CRATES or not crates[name].files:
            continue
        w(f"### {name} files")
        w("")
        for f in sorted(crates[name].files, key=lambda f: (f.kind != "src", f.path)):
            td = f" tdesktop: {td_cell(f, 2)}" if f.tdesktop else ""
            tag = "" if f.kind == "src" else f" [{f.kind}]"
            w(f"- `{f.path}` ({approx(f.lines)}){tag}: {summary_of(f, SHORT_LEN)}{td}")
        w("")

    # largest
    w("## Largest files and split points")
    w("")
    w(
        f"The {LARGEST} largest Rust files, as input for the file-splitting task. *Blocks* are the top-level "
        "items (`impl`, `mod`, `fn`) of at least 80 lines or 4 % of the file, with their approximate line "
        "ranges; for a block that holds 40 % of the file or more, its largest methods follow. Each block is a "
        "seam: a trait impl, an inline `mod` or a group of methods can move to its own file (an `impl` may live "
        "in any module of its crate). *mod files* counts the `mod x;` lines already split out of the file."
    )
    w("")
    w("| # | file | lines | mod files | blocks |")
    w("|---:|---|---:|---:|---|")
    biggest = sorted(files, key=lambda f: (-f.lines, f.path))[:LARGEST]
    for k, f in enumerate(biggest, 1):
        parts, mods = split_points(repo.texts[f.path], f.lines)
        w(f"| {k} | `{f.path}` | {approx(f.lines)} | {mods} | {'; '.join(parts) or '—'} |")
    w("")

    # undocumented
    w("## Undocumented files")
    w("")
    w(
        "Files without a `//!` module doc, largest first. Add a doc (what the file owns, and `Port of "
        "<tdesktop file>` where there is one) and regenerate."
    )
    w("")
    for f in sorted(undocumented, key=lambda f: (-f.lines, f.path)):
        w(f"- `{f.path}` ({approx(f.lines)} lines)")
    if not undocumented:
        w("None.")
    w("")
    return "\n".join(out)


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").split("\n")[0])
    ap.add_argument("--check", action="store_true", help="exit 1 when docs/CODEMAP.md is stale")
    args = ap.parse_args()
    text = render(load())
    if args.check:
        old = OUT.read_text(encoding="utf-8").replace("\r\n", "\n") if OUT.exists() else ""
        if old != text:
            print("docs/CODEMAP.md is stale: run `python -I -X utf8 tools/codemap/gen.py` and commit it.")
            return 1
        print("docs/CODEMAP.md is current.")
        return 0
    OUT.write_text(text, encoding="utf-8", newline="\n")
    print(f"wrote docs/CODEMAP.md: {len(text.encode('utf-8'))} bytes, {text.count(chr(10))} lines")
    return 0


if __name__ == "__main__":
    sys.exit(main())
