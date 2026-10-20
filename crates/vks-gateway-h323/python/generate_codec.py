#!/usr/bin/env python3
"""Regenerate ASN.1 descriptions using the separately pinned Pycrate compiler."""
from pathlib import Path
from pycrate_asn1c.asnproc import compile_text, generate_modules, PycrateGenerator

root = Path(__file__).resolve().parent
# The upstream H.245 schema contains a Windows-1252 comment. ASN.1 tokens are ASCII.
texts = [(root / 'schemas' / (name + '.asn')).read_text(encoding='latin1') for name in ('h235', 'h245', 'h225')]
compile_text(texts)
generate_modules(PycrateGenerator, str(root / 'h323_asn.py'))
