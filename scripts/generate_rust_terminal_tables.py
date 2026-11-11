#!/usr/bin/env python3
"""Regenerate the native Rust terminal's pinned keyboard/Unicode data.

Inputs are an external Ghostty reference checkout and its generated Unicode props.zig.
Build the current reference once, then locate props.zig under
the reference checkout’s .zig-cache/o/ (the file defines stage1/stage2/stage3).
Run:

    python3 scripts/generate_rust_terminal_tables.py --reference-source CHECKOUT --reference-commit SHA --unicode-properties PATH
    cargo fmt --all

This is a maintenance tool, not a build step. It executes no Zig and fetches
nothing. The generated files record the upstream source revisions; run the
native/reference differential tests after regenerating either table. Upstream
copyright and license notices are retained in crates/ghostty-vt/THIRD_PARTY_LICENSES.
"""
import argparse
import ast
import json
import re
from pathlib import Path

def generate_keymap(root, reference, commit):
    source=(reference/'src/input/function_keys.zig').read_text()
    bindings=(root/'crates/ghostty-vt/src/bindings.rs').read_text()
    ids={k:int(v) for k,v in re.findall(r'GhosttyKey_GHOSTTY_KEY_(\w+): GhosttyKey = (\d+)',bindings)}
    def key(name): return ids[(name[4:] if name.startswith('key_') else name).upper()]
    def byte(s): return '&'+repr(list(s.encode())).replace(' ', '')
    def quoted(s): return ast.literal_eval(s)
    def pc(fmt): return [(m,True,0,0,0,fmt.replace('{}',str(i+2)),None) for i,m in enumerate([1,4,5,2,3,6,7,8,9,12,13,10,11,14,15])]
    def parse_entry(s):
        mods=re.search(r'\.mods\s*=\s*\.\{([^}]+)\}',s)
        bits=sum(v for k,v in [('shift',1),('ctrl',2),('alt',4),('super',8)] if mods and re.search(r'\.'+k+r'\s*=\s*true',mods[1]))
        def enum(field):
            m=re.search(r'\.'+field+r'\s*=\s*\.(\w+)',s); return m[1] if m else 'any'
        seq=re.search(r'\.sequence\s*=\s*("(?:\\.|[^"\\])*")',s)
        alt=re.search(r'\.sequence_decbkm\s*=\s*("(?:\\.|[^"\\])*")',s)
        return (bits,'mods_empty_is_any = false' not in s, {'any':0,'normal':1,'application':2}[enum('cursor')],{'any':0,'normal':1,'application':2}[enum('keypad')],{'any':0,'set':1,'set_other':2}[enum('modify_other_keys')],quoted(seq[1]),quoted(alt[1]) if alt else None)
    all_entries={}
    for match in re.finditer(r'result\.set\(\.(\w+),\s*(.*?)\);',source,re.S):
        name, expr=match.groups(); entries=[]
        if expr.startswith('&.{'):
            for line in expr.splitlines():
                if '.sequence =' in line: entries.append(parse_entry(line))
        elif expr.startswith('kpKeys'):
            suffix, normal=map(quoted,re.findall(r'"(?:\\.|[^"\\])*"',expr))
            entries=[(0,False,0,2,0,'\x1bO'+suffix,None)]
            entries += [(m,a,c,2,o,s,t) for m,a,c,k,o,s,t in pc('\x1bO{}'+suffix)]
            entries += [(0,True,0,1,0,normal,None)]
        else:
            p=re.search(r'pcStyle\(("(?:\\.|[^"\\])*")\)',expr)
            if p: entries+=pc(quoted(p[1]))
            c=re.search(r'cursorKey\(("(?:\\.|[^"\\])*"),\s*("(?:\\.|[^"\\])*")\)',expr)
            if c: entries += [(0,True,1,0,0,quoted(c[1]),None),(0,True,2,0,0,quoted(c[2]),None)]
            elif '.sequence =' in expr: entries.append(parse_entry(expr[expr.index('Entry{'):]))
            else: raise ValueError(expr)
        all_entries[key(name)]=entries
    out=['// Ported from Ghostty input/function_keys.zig, input/kitty.zig and input/key.zig.','// Ghostty source: '+commit+'.','// This is static Rust data; builds and runtime do not require Zig.','// See THIRD_PARTY_LICENSES for the upstream copyright and license.','use super::FunctionEntry;','#[rustfmt::skip]','pub(super) fn function_entries(key: i32) -> &\'static [FunctionEntry] {','    match key {']
    for id,entries in all_entries.items():
        out.append(f'        {id} => &[')
        for m,a,c,k,o,s,t in entries:
            out.append(f'            FunctionEntry {{ mods: {m}, any_mods: {str(a).lower()}, cursor: {c}, keypad: {k}, other: {o}, sequence: {byte(s)}, backarrow: '+('Some('+byte(t)+')' if t else 'None')+' },')
        out.append('        ],')
    out+=['        _ => &[],','    }','}']
    kitty=(reference/'src/input/kitty.zig').read_text()
    out+=['pub(super) fn kitty_entry(key: i32) -> Option<(u32, char, bool)> {','    Some(match key {']
    for name,code,final,modifier in re.findall(r"\.\{\s*\.(\w+),\s*(\d+),\s*'(.)',\s*(true|false)\s*\}",kitty):
        out.append(f"        {key(name)} => ({code}, '{final}', {modifier}),")
    out+=['        _ => return None,','    })','}']
    physical=(reference/'src/input/key.zig').read_text().split('const codepoint_map:')[1].split('};')[0]
    out+=['pub(super) fn codepoint(key: i32) -> Option<u32> {','    Some(match key {']
    for char,name in re.findall(r"\.\{\s*('(?:\\.|[^'\\])*'),\s*\.(\w+)\s*\}",physical):
        out.append(f'        {key(name)} => {ord(quoted(char))},')
    out+=['        _ => return None,','    })','}']
    (root/'crates/ghostty-vt/src/keymap.rs').write_text('\n'.join(out)+'\n')


def generate_unicode(root, properties, commit, uucode_commit):
    s=properties.read_text()
    classes='other prepend regional_indicator spacing_mark l v t lv lvt zwj zwnj extended_pictographic emoji_modifier_base emoji_modifier indic_conjunct_break_extend indic_conjunct_break_linker indic_conjunct_break_consonant'.split()
    out=['// Unicode properties from the pinned Ghostty/uucode tables.','// Width and grapheme properties intentionally match the terminal printer.','// Ported from Ghostty '+commit+',','// using uucode '+uucode_commit+'.','// Builds do not execute Zig. See THIRD_PARTY_LICENSES for upstream notices.','use super::Properties;']
    for stage,length in [(1,4352),(2,None)]:
        vals=list(map(int,re.search(r'pub const stage'+str(stage)+r':.*?= \.\{(.*?)\};',s,re.S)[1].strip().strip(',').split(',')))
        if length: vals=vals[:length]
        else: vals=vals[:max(map(int,re.search(r'pub const stage1:.*?= \.\{(.*?)\};',s,re.S)[1].strip().strip(',').split(',')))+256]
        out+=['#[rustfmt::skip]',f'pub(super) static STAGE{stage}: [u{16 if stage==1 else 8}; {len(vals)}] = [']
        out+=['    '+','.join(map(str,vals[i:i+32]))+',' for i in range(0,len(vals),32)]
        out+= ['];']
    props=re.findall(r'\.width= (\d+),\s*\.width_zero_in_grapheme= (true|false),\s*\.grapheme_break= \.(\w+),\s*\.emoji_vs_base= (true|false)',s)
    out+=['pub(super) const PROPERTIES: [Properties; '+str(len(props))+'] = [']
    for w,z,g,v in props: out.append(f'    Properties {{ width: {w}, zero_in_grapheme: {z}, class: {classes.index(g)}, emoji_vs_base: {v} }},')
    out+= ['];']
    (root/'crates/ghostty-vt/src/unicode_tables.rs').write_text('\n'.join(out)+'\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--unicode-properties", type=Path, required=True)
    parser.add_argument("--reference-source", type=Path, required=True, help="External Ghostty reference checkout (maintenance only)")
    parser.add_argument("--reference-commit", required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    commit = args.reference_commit
    zon = (args.reference_source / "build.zig.zon").read_text()
    uucode_commit = re.search(r"uucode-([0-9a-f]{40})\.tar\.gz", zon)[1]
    generate_keymap(root, args.reference_source, commit)
    generate_unicode(root, args.unicode_properties, commit, uucode_commit)


if __name__ == "__main__":
    main()
