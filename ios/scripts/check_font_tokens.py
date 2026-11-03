#!/usr/bin/env python3
"""Fail when a Font token in DSMetrics.swift does not scale with Dynamic Type.

Every token in `enum DSFont` must be built from a text style (`.system(.<style>`,
`Font.custom(_:size:relativeTo:)`, or a bare text style such as `.body`) or derived
from another DSFont token. A fixed `.system(size:)` or a `custom` without `relativeTo:`
does not scale.
"""
import re
import sys
from pathlib import Path

DEFAULT_SOURCE = Path(__file__).resolve().parents[2] / "app" / "design-system" / "DSMetrics.swift"
TEXT_STYLES = (
    "largeTitle", "title", "title2", "title3", "headline", "subheadline",
    "body", "callout", "footnote", "caption", "caption2",
)
STYLE_ALTERNATIVES = "|".join(TEXT_STYLES)
SCALING_PATTERNS = (
    re.compile(r"\.system\(\s*\.(?:%s)\b" % STYLE_ALTERNATIVES),
    re.compile(r"\.custom\([^)]*\brelativeTo:\s*\.(?:%s)\b" % STYLE_ALTERNATIVES),
    re.compile(r"(?::\s*Font\s*=|->\s*Font\s*\{|:\s*Font\s*\{)\s*\.(?:%s)\b" % STYLE_ALTERNATIVES),
)
TOKEN_DECLARATION = re.compile(r"^\s*static\s+(?:let|var|func)\s+(\w+)")


def font_enum_body(source):
    start = re.search(r"enum\s+DSFont\s*\{", source)
    if start is None:
        raise ValueError("enum DSFont not found")
    depth = 1
    index = start.end()
    while index < len(source) and depth:
        depth += {"{": 1, "}": -1}.get(source[index], 0)
        index += 1
    return source[start.end():index - 1]


def declarations(body):
    """Yield (name, text) for each `static` declaration, skipping doc comments."""
    current_name, current_lines = None, []
    for line in body.splitlines():
        if line.strip().startswith("//"):
            continue
        match = TOKEN_DECLARATION.match(line)
        if match:
            if current_name:
                yield current_name, "\n".join(current_lines)
            current_name, current_lines = match.group(1), [line]
        elif current_name:
            current_lines.append(line)
    if current_name:
        yield current_name, "\n".join(current_lines)


def problems(source):
    body = font_enum_body(source)
    found = list(declarations(body))
    token_names = {name for name, _ in found}
    errors = []
    for name, text in found:
        if "Font" not in text:
            continue  # not a Font token (for example a font file name)
        derived = any(
            re.search(r"\b(?:DSFont\.)?%s\b" % re.escape(other), text.split("=", 1)[-1] if "=" in text else text)
            for other in token_names - {name}
        )
        if not (any(pattern.search(text) for pattern in SCALING_PATTERNS) or derived):
            errors.append("DSFont.%s is not built from a text style and does not scale with Dynamic Type" % name)
    return errors


def main(argv):
    path = Path(argv[1]) if len(argv) > 1 else DEFAULT_SOURCE
    errors = problems(path.read_text())
    for error in errors:
        print("%s: %s" % (path, error), file=sys.stderr)
    if not errors:
        print("font tokens scale with Dynamic Type")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
