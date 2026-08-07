"""Zero-dependency diagnostic CLI (`python -m capture_system_or_process_audio`)."""

from __future__ import annotations

import argparse
import os
import platform
import shutil
import sys
import sysconfig
from collections.abc import Sequence
from typing import Any

_BACKEND_PREFIXES = ("wasapi:", "pipewire:", "pulseaudio:", "coreaudio:", "screencapturekit:")


def _enable_ansi(stream: Any) -> bool:
    if os.environ.get("NO_COLOR") is not None or os.environ.get("TERM") == "dumb":
        return False
    if not hasattr(stream, "isatty") or not stream.isatty():
        return False
    if os.name != "nt":
        return True
    try:
        import ctypes

        kernel32 = ctypes.windll.kernel32
        handle = kernel32.GetStdHandle(-11)
        mode = ctypes.c_uint32()
        if not kernel32.GetConsoleMode(handle, ctypes.byref(mode)):
            return False
        return bool(kernel32.SetConsoleMode(handle, mode.value | 0x0004))
    except Exception:
        return bool(os.environ.get("WT_SESSION") or os.environ.get("TERM_PROGRAM"))


def _supports_unicode(stream: Any) -> bool:
    enc = getattr(stream, "encoding", None) or "utf-8"
    try:
        "╭─┬╮│├─┼┤╰┴╯…·".encode(enc)
        return True
    except UnicodeEncodeError:
        return False


def _paint(text: str, sgr: str, *, color: bool) -> str:
    return f"\x1b[{sgr}m{text}\x1b[0m" if color and sgr else text


def _truncate(text: str, limit: int, *, unicode_box: bool) -> str:
    if len(text) <= limit:
        return text
    ellipsis = "…" if unicode_box else "..."
    if limit <= len(ellipsis):
        return ellipsis[:limit]
    return text[: limit - len(ellipsis)] + ellipsis


def _plural(count: int, singular: str, plural: str | None = None) -> str:
    word = singular if count == 1 else (plural or f"{singular}s")
    return f"{count} {word}"


def _clean_endpoint_id(raw_id: str | None) -> str:
    if not raw_id:
        return "-"
    cleaned = raw_id
    for prefix in _BACKEND_PREFIXES:
        if cleaned.startswith(prefix):
            cleaned = cleaned[len(prefix) :]
            break
    for kind_prefix in ("device:", "input_device:"):
        if cleaned.startswith(kind_prefix):
            cleaned = cleaned[len(kind_prefix) :]
            break
    return cleaned or "-"


def _render_table(
    headers: Sequence[str],
    rows: Sequence[Sequence[tuple[str, str]]],
    max_widths: Sequence[int],
    *,
    color: bool,
    unicode_box: bool,
) -> list[str]:
    tl, h, top_sep, tr, v, ml, cross, mr, bl, bot_sep, br = (
        ("╭", "─", "┬", "╮", "│", "├", "┼", "┤", "╰", "┴", "╯")
        if unicode_box
        else ("+", "-", "+", "+", "|", "+", "+", "+", "+", "+", "+")
    )
    clipped_rows = [
        [(_truncate(val, lim, unicode_box=unicode_box), sgr) for (val, sgr), lim in zip(row, max_widths)]
        for row in rows
    ]
    widths = [
        max([len(hdr)] + [len(r[col][0]) for r in clipped_rows])
        for col, hdr in enumerate(headers)
    ]

    def rule(left: str, mid: str, right: str) -> str:
        inner = mid.join(h * (w + 2) for w in widths)
        return _paint(f"{left}{inner}{right}", "2", color=color)

    border_v = _paint(v, "2", color=color)
    hdr_cells = [
        f" {_paint(hdr.ljust(w), '1', color=color)} "
        for hdr, w in zip(headers, widths)
    ]
    lines = [
        rule(tl, top_sep, tr),
        f"{border_v}{border_v.join(hdr_cells)}{border_v}",
        rule(ml, cross, mr),
    ]
    for row in clipped_rows:
        cells = [
            f" {_paint(val.ljust(w), sgr, color=color)} "
            for (val, sgr), w in zip(row, widths)
        ]
        lines.append(f"{border_v}{border_v.join(cells)}{border_v}")
    lines.append(rule(bl, bot_sep, br))
    return lines


def render_diagnostics(
    cspa_mod: Any,
    *,
    active_only: bool = False,
    include_non_audio: bool = False,
    color: bool = False,
    unicode_box: bool = True,
    width: int = 108,
) -> str:
    sep_dot = " · " if unicode_box else " | "
    svc = cspa_mod.DiscoveryService()
    os_name: str = svc.os.value
    os_backends = list(getattr(cspa_mod.Backends(), os_name))
    backend_labels = [
        f"{bk.value} (primary)" if idx == 0 else bk.value
        for idx, bk in enumerate(os_backends)
    ]

    py_ver = platform.python_version()
    if int(sysconfig.get_config_var("Py_GIL_DISABLED") or 0) == 1:
        gil_on = getattr(sys, "_is_gil_enabled", lambda: False)()
        py_runtime = f"Python {py_ver}t (GIL {'on' if gil_on else 'off'})"
    else:
        py_runtime = f"Python {py_ver}"

    val_budget = max(44, width - 18)
    pkg_summary = f"capture-system-or-process-audio {cspa_mod.__version__}{sep_dot}{py_runtime}"
    if len(pkg_summary) > val_budget:
        pkg_summary = pkg_summary.replace("Python ", "Py ", 1)
    try:
        perm = cspa_mod.check_permissions()
        perm_summary = sep_dot.join(f"{e.kind.value}={e.state.value}" for e in perm.entries)
        if len(perm_summary) > val_budget:
            short_names = {"microphone": "mic", "system_audio": "system_audio", "screen_recording": "screen"}
            perm_summary = sep_dot.join(
                f"{short_names.get(e.kind.value, e.kind.value)}={e.state.value}" for e in perm.entries
            )
        perm_sgr = "32" if perm.is_granted else ("33" if not perm.has_denial else "31")
    except Exception as exc:
        perm_summary = f"unavailable ({exc})"
        perm_sgr = "33"

    summary_rows: list[Sequence[tuple[str, str]]] = [
        (("Package", "1"), (pkg_summary, "36")),
        (("Platform", "1"), (f"{os_name} ({platform.platform(terse=True)}{sep_dot}{platform.machine()})", "")),
        (("Backends", "1"), (", ".join(backend_labels) or "-", "1;36")),
        (("Permissions", "1"), (perm_summary, perm_sgr)),
    ]
    out_lines = _render_table(
        ("FIELD", "VALUE"),
        summary_rows,
        (12, val_budget),
        color=color,
        unicode_box=unicode_box,
    )

    extra = max(0, width - 92)
    col_caps = (12, 24 + extra // 3, 22 + extra // 3, 6, 8, 26 + extra - 2 * (extra // 3))

    for idx, bk in enumerate(os_backends):
        role = "primary" if idx == 0 else "secondary"
        out_lines.append("")
        try:
            single_backends = cspa_mod.Backends(**{os_name: [bk]})
            query = cspa_mod.DiscoveryQuery(
                backends=single_backends,
                dedupe=cspa_mod.DiscoveryDedupe.PER_BACKEND,
                order=cspa_mod.DiscoveryOrder.KIND_THEN_BACKEND,
                active_only=active_only,
                include_non_audio=include_non_audio,
                include_inputs=True,
            )
            catalog = cspa_mod.discover_query(query)
        except Exception as exc:
            title = _paint(f"Backend: {bk.value}", "1;36", color=color)
            status = _paint(f"0 CaptureTargets — unavailable ({exc})", "33", color=color)
            out_lines.append(f"{title} ({role}) {sep_dot.strip()} {status}")
            continue

        def_out = _clean_endpoint_id(catalog.default_output_device_id)
        def_in = _clean_endpoint_id(catalog.default_input_device_id)
        counts = sep_dot.join(
            (
                _plural(len(catalog.applications), "app"),
                _plural(len(catalog.processes), "process", "processes"),
                _plural(len(catalog.output_devices), "output"),
                _plural(len(catalog.input_devices), "input"),
            )
        )
        title = _paint(f"Backend: {bk.value}", "1;36", color=color)
        total_badge = _paint(_plural(len(catalog.targets), "CaptureTarget"), "1;32" if catalog.targets else "33", color=color)
        out_lines.append(f"{title} ({role}) {sep_dot.strip()} {total_badge} ({counts})")

        if not catalog.targets:
            empty_rows: list[Sequence[tuple[str, str]]] = [
                (("-", "2"), ("No matching capture targets", "2"), ("-", "2"), ("-", "2"), ("-", "2"), ("-", "2"))
            ]
            out_lines.extend(
                _render_table(
                    ("KIND", "NAME", "PID / ENDPOINT", "SCOPE", "STATE", "TARGET ID"),
                    empty_rows,
                    col_caps,
                    color=color,
                    unicode_box=unicode_box,
                )
            )
            continue

        target_rows: list[Sequence[tuple[str, str]]] = []
        for target in catalog.targets:
            kind_str = target.kind.value
            name_str = target.name or "-"
            is_proc_like = kind_str in ("application", "process")
            if is_proc_like:
                ident = f"pid={target.pid}" if target.pid is not None else "-"
                scope_str = target.scope.value
                if target.active_audio is True:
                    state_str, state_sgr = "playing", "1;32"
                elif target.active_audio is False:
                    state_str, state_sgr = "idle", "2"
                else:
                    state_str, state_sgr = "-", "2"
            else:
                ep_id = _clean_endpoint_id(target.device_id)
                ident = ep_id
                scope_str = "-"
                is_default = (kind_str == "device" and ep_id == def_out) or (
                    kind_str == "input_device" and ep_id == def_in
                )
                state_str, state_sgr = ("default", "36") if is_default else ("ready", "")

            target_rows.append(
                (
                    (kind_str, "35" if kind_str == "application" else ""),
                    (name_str, "1" if target.active_audio is True else ""),
                    (ident, ""),
                    (scope_str, "2" if scope_str == "-" else ""),
                    (state_str, state_sgr),
                    (target.logical_id, "2"),
                )
            )

        out_lines.extend(
            _render_table(
                ("KIND", "NAME", "PID / ENDPOINT", "SCOPE", "STATE", "TARGET ID"),
                target_rows,
                col_caps,
                color=color,
                unicode_box=unicode_box,
            )
        )

    return "\n".join(out_lines)


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python -m capture_system_or_process_audio",
        description="Display current OS audio backends and discovered CaptureTargets.",
    )
    parser.add_argument("--active-only", action="store_true", help="Show only targets currently producing audio (plus endpoints).")
    parser.add_argument("--include-non-audio", action="store_true", help="Include running processes/apps without an audio session yet.")
    parser.add_argument("--no-color", action="store_true", help="Disable ANSI terminal colors.")
    args = parser.parse_args(argv)

    import capture_system_or_process_audio as cspa

    use_color = False if args.no_color else _enable_ansi(sys.stdout)
    use_unicode = _supports_unicode(sys.stdout)
    is_tty = hasattr(sys.stdout, "isatty") and sys.stdout.isatty()
    cols = shutil.get_terminal_size((112, 24)).columns if is_tty else 112
    rendered = render_diagnostics(
        cspa,
        active_only=args.active_only,
        include_non_audio=args.include_non_audio,
        color=use_color,
        unicode_box=use_unicode,
        width=max(80, cols),
    )
    try:
        sys.stdout.write(rendered + "\n")
        sys.stdout.flush()
    except BrokenPipeError:
        return 0
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
