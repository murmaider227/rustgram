"""Scenario engine of the walk: the step language, the checks and the verdicts.

Each scenario in scenarios.toml has ONE list of steps that both clients play, so the official
and the Rustgram run cannot drift apart. A line prefixed with `[official]` or `[rustgram]` runs
only on that client (used only where the clients genuinely differ, e.g. the updater).

Steps (one per line, `#` starts a comment):
    wait S                       sleep S seconds
    size W H                     set the client area to W x H
    open TITLE                   search the chat list for TITLE and open it (checks the header)
    click T / rclick T / hover T T = text:<regex>[@region] | xy:X,Y | rel:FX,FY | right_of:<regex>:<dx>
    type TEXT                    type TEXT into the focused field
    key K [K...]                 keys or chords: enter, escape, ctrl+c, pageup ...
    scroll REGION N              N wheel notches over REGION (N > 0 scrolls up)
    peer[:var] JSON              a command for the B-side peer (tg-walk-peer serve), result in ${var}
    shot NAME                    capture; NAME becomes one official | Rustgram pair
    close                        WM_CLOSE to the main window
    restart [S]                  stop and start the client again (same scratch workdir)
    tray find|click|quit         act on the client's notification-area icon (UI Automation)
    expect KIND ...              automatic check (see Runner.expect)
    look TEXT                    something only a person can judge: marks the scenario "needs a look"

Regions: whole, list, header, history, compose, history_br (bottom-right corner of the history).
"""

from __future__ import annotations

import json
import os
import re
import shlex
import subprocess
import time
import traceback
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import numpy as np
from PIL import Image, ImageDraw

import walklib as wl

HERE = Path(__file__).resolve().parent


class Blocked(Exception):
    """The runner could not play a step (not a verdict on the client)."""


class Failed(Exception):
    """A step showed a client defect that makes the rest of the scenario meaningless."""


@dataclass
class Check:
    text: str
    ok: bool | None  # None = needs a look
    detail: str = ""


@dataclass
class ClientRun:
    client: str
    status: str = "pass"  # pass | fail | blocked | look
    checks: list[Check] = field(default_factory=list)
    shots: dict[str, Path] = field(default_factory=dict)
    error: str = ""
    seconds: float = 0.0


def find_split(img: Image.Image) -> int:
    """x of the border between the chat list and the history (0 in the one-column layout)."""
    a = np.asarray(img.convert("L"), dtype=np.int16)
    h, w = a.shape
    if w < 700:
        return 0
    rows = a[int(h * 0.15):int(h * 0.95)]
    # The border is the leftmost column where nearly every row changes colour.
    frac = (np.abs(np.diff(rows, axis=1)) > 2).mean(axis=0)
    lo, hi = 250, int(w * 0.75)
    hits = np.nonzero(frac[lo:hi] > 0.85)[0]
    return int(hits[0]) + lo + 1 if hits.size else 0


LOCKS: list[Any] = []


def login_dir(cfg: dict[str, Any], key: str) -> Path:
    """A login dir of the walk, locked for this process for the whole run. Logins are used in
    place, never copied: a copy and its original running at once share one key, and the server
    drops it (AUTH_KEY_DUPLICATED)."""
    d = Path(cfg[key])
    if not (d / "tdata").exists():
        raise SystemExit(f"{key}: {d} holds no login (mint one: tg-walk-peer mint, login_official.py)")
    LOCKS.append(wl.lock_login(d))
    return d


def make_clients(cfg: dict[str, Any], scratch: Path, names: list[str]) -> list[wl.Client]:
    """Returns (not yet started) clients on their own, locked login dirs."""
    wl.ws.ensure_outside_git_repo(scratch, "walk scratch")
    scratch.mkdir(parents=True, exist_ok=True)
    out: list[wl.Client] = []
    size = tuple(cfg.get("window", [1400, 900]))
    if "official" in names:
        wd = login_dir(cfg, "official_login")
        out.append(wl.Client(
            name="official", exe=Path(cfg["official_exe"]), workdir=wd,
            pos=tuple(cfg.get("official_pos", [-1910, 20])), size=size,
            args=["-noupdate"], log=scratch / "official_stdout.log",
        ))
    if "rustgram" in names:
        wd = login_dir(cfg, "rustgram_login")
        env = os.environ.copy()
        api_id, api_hash = wl.read_test_credentials(Path(cfg["tdesktop_dir"]) / "docs" / "api_credentials.md")
        env.update({
            "RUSTGRAM_API_ID": api_id, "RUSTGRAM_API_HASH": api_hash, "RUSTGRAM_AGENT": "1",
            "RUSTGRAM_LIVE": "1", "TGFORK_HEADLESS_AUDIO": "1",
            "PATH": cfg.get("ffmpeg_bin", "") + os.pathsep + env.get("PATH", ""),
            "RUST_LOG": cfg.get("rust_log", "warn"),
        })
        out.append(wl.Client(
            name="rustgram", exe=Path(cfg["rustgram_exe"]), workdir=wd,
            pos=tuple(cfg.get("rustgram_pos", [1930, 20])), size=size, env=env,
            args=["-testmode"], log=scratch / "rustgram_stdout.log",
        ))
    return out


class Ctx:
    """State of one scenario on one client."""

    def __init__(self, runner: "Runner", client: wl.Client, sid: str, out: Path) -> None:
        self.r, self.c, self.sid, self.out = runner, client, sid, out
        self.run = ClientRun(client.name)
        self.img: Image.Image | None = None
        self.img_path: Path | None = None
        self._ocr: dict[str, list[wl.OcrLine]] = {}
        self.vars: dict[str, Any] = dict(runner.cfg.get("vars", {}))

    def capture(self, name: str | None = None) -> Image.Image:
        path = self.out / f"{self.sid}_{self.c.name}_{name or '_tmp'}.png"
        self.img = self.c.shot(path)
        self.img_path = path
        if name:
            self.run.shots[name] = path
        return self.img

    def lines(self) -> list[wl.OcrLine]:
        if self.img_path is None:
            self.capture()
        assert self.img_path
        key = str(self.img_path) + str(self.img_path.stat().st_mtime_ns)
        if key not in self._ocr:
            self._ocr[key] = wl.ocr([self.img_path]).get(str(self.img_path), [])
        return self._ocr[key]

    def region(self, name: str) -> tuple[int, int, int, int]:
        if self.img is None:
            self.capture()
        assert self.img is not None
        w, h = self.img.size
        split = find_split(self.img)
        # Both clients draw their own title bar (~25 px) above the chat header.
        top, bottom = 90, 60
        regions = {
            "whole": (0, 0, w, h),
            "list": (0, 0, split or w, h),
            "header": (split, 0, w, top),
            "history": (split, top, w, h - bottom),
            "compose": (split, h - bottom, w, h),
            "history_br": (max(split, w - 140), int(h * 0.45), w, h - bottom),
            # Where tdesktop's corner buttons (scroll down, "@", reactions) sit.
            "corner": (max(split, w - 80), h - bottom - 280, w, h - bottom),
        }
        if name not in regions:
            raise Blocked(f"unknown region {name}")
        return regions[name]

    def find(self, pattern: str, region: str = "whole", fresh: bool = True) -> wl.OcrLine | None:
        if fresh:
            self.capture()
        x0, y0, x1, y1 = self.region(region)
        for ln in self.lines():
            cx, cy = ln.x + ln.w // 2, ln.y + ln.h // 2
            if x0 <= cx < x1 and y0 <= cy < y1 and wl.text_matches(pattern, ln.text):
                return ln
        return None

    def target(self, spec: str) -> tuple[int, int]:
        if spec.startswith("xy:"):
            x, y = spec[3:].split(",")
            return int(x), int(y)
        if spec.startswith(("br:", "hist_bl:")):
            # Offsets from the bottom-right corner of the window, or from the bottom-left of
            # the history column (compose bar buttons).
            dx, dy = (int(v) for v in spec.split(":", 1)[1].split(","))
            w, h = self.c.client_size()
            if spec.startswith("br:"):
                return w - dx, h - dy
            self.capture()
            return self.region("history")[0] + dx, h - dy
        if spec.startswith("rel:"):
            fx, fy = spec[4:].split(",")
            w, h = self.c.client_size()
            return int(float(fx) * w), int(float(fy) * h)
        if spec.startswith("text:"):
            body = spec[5:]
            pattern, region = (body.rsplit("@", 1) + ["whole"])[:2] if "@" in body else (body, "whole")
            for _ in range(3):
                ln = self.find(pattern, region)
                if ln:
                    return ln.x + ln.w // 2, ln.y + ln.h // 2
                time.sleep(1.0)
            raise Blocked(f"text not on screen: {pattern}")
        if spec.startswith(("right_of:", "below:", "left_of:")):
            kind, pattern, *offs = spec.split(":")
            dx = int(offs[0]) if offs else 0
            dy = int(offs[1]) if len(offs) > 1 else 0
            ln = None
            for _ in range(3):
                ln = self.find(pattern)
                if ln:
                    break
                time.sleep(1.0)
            if not ln:
                raise Blocked(f"text not on screen: {pattern}")
            if kind == "right_of":
                return ln.x + ln.w + dx, ln.y + ln.h // 2 + dy
            if kind == "left_of":
                return ln.x - dx, ln.y + ln.h // 2 + dy
            return ln.x + dx, ln.y + ln.h + dy
        raise Blocked(f"bad target {spec}")

    def check(self, text: str, ok: bool | None, detail: str = "") -> None:
        self.run.checks.append(Check(text, ok, detail))


def sub_vars(s: str, vars: dict[str, Any]) -> str:
    def rep(m: re.Match[str]) -> str:
        cur: Any = vars
        for part in m.group(1).split("."):
            cur = cur[part]
        return str(cur)

    return re.sub(r"\$\{([a-zA-Z0-9_.]+)\}", rep, s)


class Runner:
    def __init__(self, cfg: dict[str, Any], out: Path, scratch: Path, client_names: list[str]) -> None:
        self.cfg, self.out, self.scratch = cfg, out, scratch
        self.clients = make_clients(cfg, scratch, client_names)
        self.peer: wl.Peer | None = None
        self.actor: wl.Client | None = None
        self.actor_chat = ""
        self.actor_dirty = False
        self.actor_field: dict[str, tuple[int, int]] = {}
        if cfg.get("counter_login"):
            wd = login_dir(cfg, "counter_login")
            self.actor = wl.Client(name="official", exe=Path(cfg["official_exe"]), workdir=wd,
                                   pos=tuple(cfg.get("counter_pos", [3340, 100])), size=tuple(cfg.get("counter_size", [480, 760])),
                                   args=["-noupdate"], log=scratch / "counter_stdout.log")
        self.created: list[tuple[str, str, int]] = []

    def check_keys(self) -> None:
        """Every process of the walk must hold its OWN authorization: the server drops a key that
        two processes use at the same time (AUTH_KEY_DUPLICATED), which logged account A out on
        2026-11-27 when Rustgram and the peer ran on copies of one working dir."""
        def key_of(d: str) -> bytes:
            try:
                return (Path(d) / "tdata" / "key_datas").read_bytes()
            except OSError:
                return b""

        keys = [k for k in ("rustgram_login", "peer_login", "peer_login_other") if self.cfg.get(k)]
        seen: dict[bytes, str] = {}
        for k in keys:
            data = key_of(self.cfg[k])
            if data and data in seen:
                raise SystemExit(f"refusing to run: {k} and {seen[data]} are copies of one login; "
                                 "mint a separate login for each (tg-walk-peer mint)")
            seen[data] = k

    def start(self) -> None:
        self.check_keys()
        env = os.environ.copy()
        env["TDESKTOP_DIR"] = self.cfg["tdesktop_dir"]
        # The peer locks its own login dirs; the runner only checks they are free.
        for k in ("peer_login", "peer_login_other"):
            if self.cfg.get(k):
                wl.lock_login(Path(self.cfg[k])).close()
        main = self.cfg.get("main", "a")
        if self.cfg.get("peer_login_other"):
            a_dir, b_dir = (self.cfg["peer_login"], self.cfg["peer_login_other"]) if main == "a" \
                else (self.cfg["peer_login_other"], self.cfg["peer_login"])
            argv = ["serve", "--a", a_dir, "--b", b_dir, "--state", self.cfg["state"], "--media", self.cfg["media"]]
        else:
            argv = ["solo-serve", "--main", self.cfg["peer_login"], "--who", main, "--state", self.cfg["state"]]
        self.peer = wl.Peer(Path(self.cfg["peer_exe"]), argv, env, self.scratch / "peer.log")
        self.peer(op="refresh")
        for c in self.clients:
            c.start()
        if self.actor is not None:
            self.actor.start()
        time.sleep(float(self.cfg.get("startup_wait_s", 20)))
        self.match_theme()
        self.normalize_columns()

    def match_theme(self) -> None:
        """Switches the official client to Night Mode when the Rustgram copy is dark (the test
        workdir keeps night mode), so both clients are compared in the same theme."""
        by = {c.name: c for c in self.clients}
        if "official" not in by or "rustgram" not in by:
            return

        def dark(img: Image.Image) -> bool:
            return float(np.asarray(img.convert("L").crop((80, 200, 300, 600))).mean()) < 110

        if dark(by["rustgram"].shot()) and not dark(by["official"].shot()):
            off = by["official"]
            ctx = Ctx(self, off, "setup", self.scratch)
            off.click(36, 52)
            time.sleep(1.2)
            ln = ctx.find(r"^Night Mode")
            if ln:
                off.click(ln.x + ln.w // 2, ln.y + ln.h // 2)
                time.sleep(2.5)
            off.key("escape")
            time.sleep(0.5)

    def normalize_columns(self) -> None:
        """Drags the official client's chat-list border to Rustgram's, so both history columns
        have the same width (the official copy keeps a wider list from its own settings)."""
        by = {c.name: c for c in self.clients}
        if "official" not in by or "rustgram" not in by:
            return
        want = find_split(by["rustgram"].shot())
        for _ in range(2):
            off = by["official"]
            have = find_split(off.shot())
            if not want or not have or abs(have - want) < 4:
                return
            y = off.client_size()[1] // 2
            off.drag(have, y, want, y)
            time.sleep(1.0)

    def stop(self) -> None:
        if self.actor is not None:
            if self.actor_dirty:
                try:
                    self.actor_clear()
                except Exception:  # noqa: BLE001
                    pass
            self.actor.stop()
        for c in self.clients:
            c.stop()
            # OB-10 stages a stale update package in the login dir; never leave it behind.
            tup = c.workdir / "tupdates"
            if tup.exists():
                import shutil

                shutil.rmtree(tup, ignore_errors=True)
        if self.peer:
            # Messages the walk sent are deleted again, so the fixture stays the same size.
            # Everything the walk sends carries one of these markers; delete it again so the
            # fixture keeps its shape (also after an interrupted run).
            for chat in ("pm", "basic", "super", "saved"):
                for who, marker in (("a", "walk tmp"), ("b", "walk filler"), ("b", "Walk check:"),
                                    ("b", "Walker, a mention"), ("b", "Walker, mention marker"),
                                    ("b", "a mention for the walk check"), ("b", "mention marker check")):
                    try:
                        if self.actor is not None:
                            # One-account mode: the walk account removes the other side's walk
                            # messages where it may (owner of the supergroup, private chat).
                            self.peer(op="cleanup", who="a", chat=chat, marker=marker, any=True)
                        else:
                            self.peer(op="cleanup", who=who, chat=chat, marker=marker)
                    except Exception:  # noqa: BLE001
                        pass
            for who, chat, mid in self.created:
                try:
                    self.peer(op="delete", who=who, chat=chat, ids=[mid])
                except Exception:  # noqa: BLE001
                    pass
            self.peer.close()

    def reset(self, c: wl.Client) -> None:
        if not c.alive():
            c.start()
            time.sleep(15)
        if not c.find_window():
            raise Blocked("client has no visible window")
        c.place(tuple(self.cfg.get("window", [1400, 900])))
        ctx = Ctx(self, c, "reset", self.scratch)
        crash = ctx.find(r"not closed properly")
        if crash:
            # After an unclean exit Rustgram asks first. "Restart" would hand over to a new process
            # the runner does not own, so quit from that screen and start the client again.
            btn = ctx.find(r"^QUIT$|^Quit$", fresh=False)
            if btn:
                c.click(btn.x + btn.w // 2, btn.y + btn.h // 2)
                try:
                    if c.proc:
                        c.proc.wait(timeout=10)
                except Exception:  # noqa: BLE001
                    pass
            c.stop()
            c.start()
            time.sleep(15)
            c.place(tuple(self.cfg.get("window", [1400, 900])))
            if ctx.find(r"not closed properly"):
                raise Blocked(f"{c.name} keeps showing its unclean-exit screen")
        self.close_panel(ctx)
        if ctx.find(r"^Start Messaging|Authorization error"):
            raise Blocked(f"{c.name} is logged out (login screen); every check would be meaningless")
        for _ in range(3):
            c.key("escape")
            time.sleep(0.2)

    def play(self, sc: dict[str, Any]) -> dict[str, Any]:
        sid = sc["id"]
        runs: dict[str, ClientRun] = {}
        for c in self.clients:
            if sc.get("only") and c.name not in sc["only"]:
                continue
            ctx = Ctx(self, c, sid, self.out)
            t0 = time.time()
            try:
                self.reset(c)
                for raw in sc["steps"].splitlines():
                    line = raw.split(" #", 1)[0].strip()
                    if not line or line.startswith("#"):
                        continue
                    m = re.match(r"^\[(official|rustgram)\]\s+(.*)$", line)
                    if m:
                        if m.group(1) != c.name:
                            continue
                        line = m.group(2)
                    self.step(ctx, sub_vars(line, ctx.vars))
            except Blocked as e:
                ctx.run.status, ctx.run.error = "blocked", str(e)
            except Failed as e:
                ctx.check(str(e), False)
            except Exception as e:  # noqa: BLE001
                ctx.run.status, ctx.run.error = "blocked", f"{type(e).__name__}: {e}"
                traceback.print_exc()
            ctx.run.seconds = time.time() - t0
            if ctx.run.status != "blocked":
                if any(ch.ok is False for ch in ctx.run.checks):
                    ctx.run.status = "fail"
                elif any(ch.ok is None for ch in ctx.run.checks):
                    ctx.run.status = "look"
            runs[c.name] = ctx.run
        return self.verdict(sc, runs)

    def verdict(self, sc: dict[str, Any], runs: dict[str, ClientRun]) -> dict[str, Any]:
        rg, off = runs.get("rustgram"), runs.get("official")
        if rg is None:
            status, note = "blocked", "Rustgram not run"
        else:
            status = rg.status
            failed = [f"{ch.text} ({ch.detail})" if ch.detail else ch.text for ch in rg.checks if ch.ok is False]
            note = rg.error or "; ".join(failed)
        # A check the reference client fails too is a broken check, not a Rustgram verdict.
        if rg and off and off.status in ("fail", "blocked") and rg.status in ("pass", "fail"):
            bad = [ch.text for ch in off.checks if ch.ok is False] or [off.error]
            status, note = "look", "reference client did not pass either: " + "; ".join(bad)
        pairs = self.pairs(sc["id"], runs)
        return {
            "id": sc["id"], "title": sc["title"], "status": status, "note": note,
            "bug": sc.get("bug", ""), "pairs": [str(p) for p in pairs],
            "clients": {k: {"status": v.status, "error": v.error, "seconds": round(v.seconds, 1),
                            "checks": [{"text": ch.text, "ok": ch.ok, "detail": ch.detail} for ch in v.checks]}
                        for k, v in runs.items()},
        }

    def pairs(self, sid: str, runs: dict[str, ClientRun]) -> list[Path]:
        off, rg = runs.get("official"), runs.get("rustgram")
        names: list[str] = []
        for r in (off, rg):
            for n in (r.shots if r else {}):
                if n not in names:
                    names.append(n)
        return [make_pair(off.shots.get(n) if off else None, rg.shots.get(n) if rg else None,
                          self.out / f"{sid}_{n}_pair.png") for n in names]

    def step(self, ctx: Ctx, line: str) -> None:
        cmd, _, rest = line.partition(" ")
        rest = rest.strip()
        c = ctx.c
        if cmd == "wait":
            time.sleep(float(rest))
        elif cmd == "size":
            w, h = (int(v) for v in rest.split())
            c.place((w, h))
            time.sleep(1.0)
        elif cmd == "open":
            self.open_chat(ctx, rest)
        elif cmd in ("click", "rclick", "hover"):
            x, y = ctx.target(rest)
            if cmd == "hover":
                c.move(x, y)
            else:
                c.click(x, y, right=cmd == "rclick")
            time.sleep(0.6)
        elif cmd == "type":
            c.type(rest)
            time.sleep(0.3)
        elif cmd == "key":
            for k in rest.split():
                c.key(k)
        elif cmd == "scroll":
            region, n = rest.split()
            ctx.capture()
            x0, y0, x1, y1 = ctx.region(region)
            c.wheel((x0 + x1) // 2, (y0 + y1) // 2, int(n))
            time.sleep(0.8)
        elif cmd == "seek":
            # Goes to the newest message, then scrolls up in steps shorter than a screen until
            # the text is on screen, so nothing is skipped; fails at the top of the chat.
            if ctx.find(rest, "history"):
                return
            x0, y0, x1, y1 = ctx.region("history")
            cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
            c.wheel(cx, cy, -400)
            time.sleep(1.5)
            prev, still = None, 0
            for _ in range(160):
                if ctx.find(rest, "history"):
                    return
                assert ctx.img is not None
                cur = ctx.img.crop((x0, y0, x1, y1))
                if prev is not None and wl.diff_ratio(prev, cur) < 0.002:
                    still += 1
                    if still >= 3:
                        break
                else:
                    still = 0
                prev = cur
                c.wheel(cx, cy, 3)
                time.sleep(0.6)
            raise Failed(f"message not reachable by scrolling: {rest}")
        elif cmd.startswith("peer"):
            self.peer_step(ctx, cmd, rest)
        elif cmd == "shot":
            ctx.capture(rest)
        elif cmd == "close":
            c.close_window()
            time.sleep(2.0)
        elif cmd == "stop":
            c.stop()
        elif cmd == "stage_update":
            stage_update(c.workdir, rest)
        elif cmd == "hold":
            # Press, keep the button down for S seconds, release (voice recording).
            spec, _, secs = rest.rpartition(" ")
            x, y = ctx.target(spec)
            c.hold(x, y, float(secs))
            time.sleep(1.0)
        elif cmd == "clipboard_clear":
            ctx.vars["_clip_saved"] = wl.clipboard_get()
            wl.clipboard_set("")
        elif cmd == "select":
            # Drag-select text from the start of one OCR line to the end of another.
            a, b = shlex.split(rest)
            la, lb = ctx.find(a, "history"), ctx.find(b, "history", fresh=False)
            if not la or not lb:
                raise Blocked("text to select not on screen")
            c.drag(la.x + 2, la.y + la.h // 2, lb.x + lb.w - 2, lb.y + lb.h // 2)
            time.sleep(0.6)
        elif cmd == "restart":
            c.stop()
            c.start()
            time.sleep(float(rest or 15))
        elif cmd == "tray":
            tray(ctx, rest)
        elif cmd == "expect":
            self.expect(ctx, rest)
        elif cmd == "look":
            ctx.check(rest, None, "needs a look at the pair")
        else:
            raise Blocked(f"unknown step: {cmd}")

    def open_chat(self, ctx: Ctx, title: str) -> None:
        c = ctx.c
        c.key("escape")
        c.key("escape")
        ln = ctx.find(r"^Search", "list")
        if ln is None:
            raise Blocked("search field not found")
        c.click(ln.x + ln.w // 2, ln.y + ln.h // 2)
        time.sleep(0.5)
        c.type(title)
        time.sleep(2.0)
        hit = None
        for _ in range(3):
            ctx.capture()
            x1 = ctx.region("list")[2]
            for l2 in ctx.lines():
                if l2.y > ln.y + ln.h + 10 and l2.x < x1 and wl.text_matches(re.escape(title), l2.text):
                    hit = l2
                    break
            if hit:
                break
            time.sleep(1.0)
        if hit is None:
            raise Blocked(f"chat '{title}' not found by search")
        c.click(hit.x + hit.w // 2, hit.y + hit.h // 2)
        for _ in range(4):
            time.sleep(1.5)
            if ctx.find(re.escape(title), "header"):
                time.sleep(1.5)  # the remembered profile column slides in after the chat
                self.close_panel(ctx)
                return
        raise Blocked(f"chat '{title}' did not open")

    def close_panel(self, ctx: Ctx) -> None:
        """Closes the profile shown as a third column. tdesktop remembers it per window and opens
        it with every chat until its x is clicked while a chat is open; every scenario must start
        with the same two columns (the x sits at the panel's top right)."""
        c = ctx.c
        w, _h = c.client_size()
        for attempt in range(3):
            ctx.capture()
            right = [ln for ln in ctx.lines() if ln.x > w - 360]
            # The emoji/sticker panel docked as a column (tdesktop keeps it once opened at a wide
            # window): its toggle is the smiley in the message field, left of the column.
            if any(wl.text_matches(r"^GIFs$", ln.text) for ln in right) and \
                    any(wl.text_matches(r"^Stickers$", ln.text) for ln in right):
                left = min(ln.x for ln in right if wl.text_matches(r"^Emoji$|^Stickers$|^GIFs$", ln.text))
                c.click(left - 90, _h - 24)
                time.sleep(1.2)
                continue
            panel = [ln for ln in right
                     if wl.text_matches(r"members$|^Description$|photos$|^Notifications$|shared links|^Mobile$|groups? in common",
                                        ln.text)]
            if not panel:
                return
            cross = [ln for ln in ctx.lines() if ln.x > w - 70 and 25 < ln.y < 130 and ln.text.strip() in ('x', 'X', '×')]
            # tdesktop draws the x at y~52, Rustgram at y~80.
            fallback = [(w - 28, 52), (w - 28, 80), (w - 22, 66)][attempt]
            x, y = ((cross[0].x + cross[0].w // 2, cross[0].y + cross[0].h // 2) if cross and attempt == 0 else fallback)
            c.move(x, y)
            time.sleep(0.3)
            c.click(x, y)
            time.sleep(1.2)

    def peer_step(self, ctx: Ctx, cmd: str, rest: str) -> None:
        assert self.peer
        args = json.loads(rest)
        args.setdefault("who", "b")
        if args["who"] == "b" and self.actor is not None:
            self.actor_op(args)
            return
        res = self.peer(**args)
        if not res.get("ok"):
            raise Blocked(f"peer {args.get('op')}: {res.get('error')}")
        if args.get("op") == "send" and "id" in res:
            self.created.append((args["who"], args.get("chat", "pm"), int(res["id"])))
        if ":" in cmd:
            ctx.vars[cmd.split(":", 1)[1]] = res

    # -- the other side, played through a third client ("counter") -------------------------------
    ACTOR_TITLES = {"pm": "Walk Peer B", "super": "Walk Supergroup", "basic": "Walk Basic Group",
                    "channel": "Walk Channel"}

    def actor_open(self, chat: str) -> Ctx:
        assert self.actor is not None
        ctx = Ctx(self, self.actor, "actor", self.scratch)
        title = self.cfg.get("counter_titles", self.ACTOR_TITLES).get(chat)
        if not title:
            raise Blocked(f"the other side has no chat {chat}")
        if self.actor_chat != chat:
            self.open_chat(ctx, title)
            self.actor_chat = chat
        return ctx

    def actor_clear(self) -> None:
        """Empties the counter client's message field (Qt ignores a posted Ctrl+A)."""
        assert self.actor is not None
        self.actor.key("end")
        for _ in range(45):
            wl.user32.PostMessageW(self.actor.hwnd, wl.WM_KEYDOWN, wl.VK["backspace"], 1)
            wl.user32.PostMessageW(self.actor.hwnd, wl.WM_CHAR, 8, 1)
            wl.user32.PostMessageW(self.actor.hwnd, wl.WM_KEYUP, wl.VK["backspace"], 0xC0000001)
            time.sleep(0.01)

    def actor_op(self, args: dict[str, Any]) -> None:
        """What the peer's B side did before, done by the counter client in its own window
        (no second Rustgram-readable login exists for that account)."""
        op, chat = args.get("op"), args.get("chat", "pm")
        a = self.actor
        assert a is not None
        if op == "react":
            raise Blocked("reactions by the other side need the second account's own login (TGB-3158)")
        if op not in ("send", "typing", "cancel_typing"):
            raise Blocked(f"the other side cannot do {op} through its window")
        ctx = self.actor_open(chat)
        # The field shows its placeholder only while empty: remember where it was.
        if chat not in self.actor_field:
            field = ctx.find(r"Write a message|Broadcast", "compose")
            if field is None:
                # A draft left in the field hides the placeholder: clear it and look again.
                x0, y0, x1, y1 = ctx.region("compose")
                a.click(x0 + 150, (y0 + y1) // 2)
                self.actor_clear()
                time.sleep(0.8)
                field = ctx.find(r"Write a message|Broadcast", "compose")
            if field is None:
                raise Blocked("the other side's message field is not on screen")
            self.actor_field[chat] = (field.x + field.w // 2, field.y + field.h // 2)
        a.click(*self.actor_field[chat])
        time.sleep(0.3)
        if op == "typing":
            for ch in "walk typing check":
                a.type(ch)
                time.sleep(0.08)
            self.actor_dirty = True
        elif op == "cancel_typing":
            self.actor_clear()
            self.actor_dirty = False
        else:
            self.actor_clear()
            self.actor_dirty = False
            text = str(args.get("text", "walk check"))
            if args.get("mention"):
                # "@" lists the group's members (tdesktop FieldAutocomplete); Tab takes the first,
                # which is the walk account: a mention without a username.
                a.type("@")
                time.sleep(1.5)
                a.key("tab")
                time.sleep(0.5)
                text = " " + text.split(" ", 1)[-1]
            a.type(text)
            time.sleep(0.3)
            a.key("enter")
        time.sleep(0.5)

    def expect(self, ctx: Ctx, rest: str) -> None:
        kind, _, args = rest.partition(" ")
        c = ctx.c
        text = f"{kind} {args}".strip()
        if kind in ("text", "notext"):
            region, _, pattern = args.partition(" ")
            ok = False
            for _ in range(3):
                found = ctx.find(pattern, region) is not None
                ok = found if kind == "text" else not found
                if ok:
                    break
                time.sleep(1.0)
            ctx.check(text, ok)
        elif kind in ("samecol", "rightof"):
            p1, p2 = shlex.split(args)
            ctx.capture()
            l1, l2 = ctx.find(p1, "history", fresh=False), ctx.find(p2, "history", fresh=False)
            if not l1 or not l2:
                raise Blocked(f"text for {kind} not on screen ({p1 if not l1 else p2})")
            if kind == "samecol":
                ctx.check(text, abs(l1.x - l2.x) < 60, f"x {l1.x} vs {l2.x}")
            else:
                ctx.check(text, l1.x > l2.x + 100, f"x {l1.x} vs {l2.x}")
        elif kind == "alive":
            ctx.check(text, c.alive())
        elif kind == "exited":
            deadline = time.time() + 10
            while c.alive() and time.time() < deadline:
                time.sleep(0.5)
            ctx.check(text, not c.alive())
        elif kind == "hidden":
            ctx.check(text, c.alive() and not c.window_visible())
        elif kind == "visible":
            ok = False
            for _ in range(10):
                if c.window_visible():
                    ok = True
                    break
                time.sleep(0.5)
            ctx.check(text, ok)
        elif kind == "server":
            m = re.match(r"^(\{.*\})\s+(\S+)\s+(\S+)\s+(.+)$", args)
            if not m:
                raise Blocked(f"bad server check: {args}")
            q, field_, op, value = m.groups()
            assert self.peer
            res = self.peer(**json.loads(q))
            got = res.get(field_)
            want: Any = json.loads(value)
            ok = {"==": got == want, "!=": got != want,
                  ">": got is not None and got > want, "<": got is not None and got < want}[op]
            ctx.check(text, ok, f"server {field_}={got}")
        elif kind in ("blank", "notblank"):
            ctx.capture()
            x0, y0, x1, y1 = ctx.region(args)
            assert ctx.img
            a = np.asarray(ctx.img.crop((x0, y0, x1, y1)).convert("L"), dtype=np.float32)
            share = float((np.abs(a - np.median(a)) > 40).mean())
            ok = share < 0.01 if kind == "blank" else share >= 0.01
            ctx.check(text, ok, f"contrast share {share:.3f}")
        elif kind in ("badge", "nobadge"):
            # Unread counter badges are bright blue in both themes; wallpapers and incoming
            # bubbles have no such pixels. Region defaults to the corner buttons.
            ctx.capture()
            region, _, lim = (args or "corner").partition(" ")
            ctx.vars["_badge_limit"] = int(lim) if lim else 40
            x0, y0, x1, y1 = ctx.region(region)
            assert ctx.img
            a = np.asarray(ctx.img.crop((x0, y0, x1, y1)).convert("RGB"), dtype=np.int16)
            r, g, b = a[..., 0], a[..., 1], a[..., 2]
            n = int(((b >= 150) & (b - r >= 60) & (b - g >= 20)).sum())
            limit = int(ctx.vars.get("_badge_limit", 40))
            ok = n >= limit if kind == "badge" else n < limit
            ctx.check(text, ok, f"badge pixels {n}")
        elif kind == "above":
            p1, p2 = shlex.split(args)
            ctx.capture()
            l1, l2 = ctx.find(p1, "list", fresh=False), ctx.find(p2, "list", fresh=False)
            if not l1 or not l2:
                raise Blocked(f"text for above not on screen ({p1 if not l1 else p2})")
            ctx.check(text, l1.y < l2.y, f"y {l1.y} vs {l2.y}")
        elif kind == "listbadge":
            # The unread counter on a chat-list row equals the server's unread count.
            m = re.match(r'^"([^"]+)"\s+(\{.*\})$', args)
            if not m:
                raise Blocked(f"bad listbadge check: {args}")
            title, q = m.groups()
            assert self.peer
            want = self.peer(**json.loads(q)).get("unread")
            row = ctx.find(title, "list")
            if row is None:
                raise Blocked(f"row {title} not in the list")
            x1 = ctx.region("list")[2]
            got = None
            for ln in ctx.lines():
                t = ln.text.replace(" ", "")
                if (row.y - 5 <= ln.y <= row.y + 50 and ln.x > x1 - 90 and t.isdigit()):
                    got = int(t)
            ctx.check(text, got == want, f"badge {got}, server unread {want}")
        elif kind == "nostaged":
            ctx.check(text, not (c.workdir / "tupdates" / "temp" / "ready").exists())
        elif kind == "clipboard":
            got = wl.clipboard_get()
            if "_clip_saved" in ctx.vars:
                wl.clipboard_set(ctx.vars.pop("_clip_saved") or "")
            ok = bool(got) and wl.text_matches(args, got)
            ctx.check(text, ok, f"clipboard has {len(got or '')} chars")
        elif kind == "fullscreen":
            wins = wl.wm.list_visible_windows_for_pid(c.pid, min_w=300, min_h=200)
            mon = wl.monitor_rect(c.hwnd)
            full = any(w.rect == mon for w in wins)
            ctx.check(text, full, f"windows {[w.rect for w in wins]} monitor {mon}")
        elif kind in ("changed", "same"):
            s1, s2, region = args.split()
            p1, p2 = ctx.run.shots.get(s1), ctx.run.shots.get(s2)
            if not p1 or not p2:
                raise Blocked("shots for compare missing")
            i1, i2 = Image.open(p1), Image.open(p2)
            ctx.img = i1
            box = ctx.region(region)
            d = wl.diff_ratio(i1.crop(box), i2.crop(box))
            ok = d > 0.002 if kind == "changed" else d <= 0.002
            ctx.check(text, ok, f"diff {d:.4f}")
        else:
            raise Blocked(f"unknown expect {kind}")


def stage_update(workdir: Path, version: str) -> None:
    """Puts a staged update package of `version` into a scratch working dir, in the layout
    tg-updater reads (tupdates/temp/{ready, tdata/version}; numeric = major*1e6+minor*1e3+patch).
    The package holds no real executable: it is never meant to be installed."""
    import struct

    core = version.split("-", 1)[0]
    major, minor, patch_ = (int(x) for x in core.split("."))
    numeric = major * 1_000_000 + minor * 1_000 + patch_
    temp = workdir / "tupdates" / "temp"
    (temp / "tdata").mkdir(parents=True, exist_ok=True)
    text = version.encode("utf-16-le")
    (temp / "tdata" / "version").write_bytes(struct.pack("<II", numeric, len(text)) + text)
    (temp / "rustgram.exe").write_bytes(b"staged by the walk; not an executable")
    (temp / "ready").write_bytes(b"1")


def make_pair(a: Path | None, b: Path | None, out: Path) -> Path:
    imgs = [Image.open(p).convert("RGB") if p and p.exists() else None for p in (a, b)]
    w = max((i.width for i in imgs if i), default=400)
    h = max((i.height for i in imgs if i), default=300)
    canvas = Image.new("RGB", (w * 2 + 12, h + 28), (40, 40, 40))
    d = ImageDraw.Draw(canvas)
    for k, (img, label) in enumerate(zip(imgs, ("official", "Rustgram"))):
        x = k * (w + 12)
        d.text((x + 6, 6), label, fill=(255, 255, 255))
        if img:
            canvas.paste(img, (x, 28))
        else:
            d.text((x + 20, 60), "(not captured)", fill=(255, 120, 120))
    canvas.save(out)
    return out


def tray(ctx: Ctx, action: str) -> None:
    """Acts on the client's notification-area icon.

    `find` locates the icon in the taskbar through UI Automation and saves a picture of it.
    `click` / `quit` send the app's hidden tray window the notify-icon callback Windows sends for
    a left click / a right click (WM_APP+1, NOTIFYICON_VERSION_4, LOWORD = WM_LBUTTONUP or
    WM_CONTEXTMENU), then `quit` picks "Quit" in the menu that opens. Windows 11 tray buttons
    support no UIA Invoke, and a real click would move the user's mouse.
    Rustgram only: the user's own Telegram has the same icon name as the official copy."""
    if action in ("click", "menu"):
        if ctx.c.name != "rustgram":
            raise Blocked("tray clicks run in Rustgram only")
        wins = [w for w in wl.wm.list_all_windows_for_pid(ctx.c.pid)
                if wl.window_class(w.hwnd) == "RustgramTrayIconWindow"]
        if not wins:
            raise Blocked("no tray window (RustgramTrayIconWindow) in the process")
        event = 0x0202 if action == "click" else 0x007B
        wl.user32.PostMessageW(wins[0].hwnd, 0x8001, 0, (1 << 16) | event)
        time.sleep(1.5)
        if action == "menu":
            menu = wl.find_menu_window(ctx.c.pid)
            if not menu:
                raise Blocked("tray menu did not open")
            shot = ctx.out / f"{ctx.sid}_{ctx.c.name}_traymenu.png"
            img = wl.capture_screen_window(menu, shot)
            ctx.run.shots["traymenu"] = shot
            lines = wl.ocr([shot]).get(str(shot), [])
            ctx.check("tray menu has a Quit item", any(wl.text_matches(r"^Quit", ln.text) for ln in lines))
            ctx.check("tray menu has no debug items", not any(wl.text_matches(r"Simulate", ln.text) for ln in lines))
            # Close the menu again (Escape to its modal loop). A posted click on "Quit" cannot be
            # told apart from a dismissed menu, so quitting is checked with Ctrl+Q instead.
            wl.user32.PostMessageW(menu, 0x0100, 0x1B, 1)
            wl.user32.PostMessageW(menu, 0x0101, 0x1B, 0xC0000001)
            time.sleep(2.0)
        return
    name = {"official": "Telegram", "rustgram": "Rustgram"}[ctx.c.name]
    icon = ctx.out / f"{ctx.sid}_{ctx.c.name}_trayicon.png"
    res = subprocess.run(
        ["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", str(HERE / "tray_uia.ps1"),
         "-Name", name, "-Action", action, "-Out", str(icon)],
        capture_output=True, text=True, timeout=60,
    )
    out = (res.stdout or "").strip()
    if res.returncode != 0 or not out.startswith("OK"):
        raise Blocked(f"tray {action}: {(out or res.stderr.strip())[:200]}")
    if action == "find" and icon.exists():
        ctx.run.shots["trayicon"] = icon
    time.sleep(1.5)
