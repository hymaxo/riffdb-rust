"""asmfn.py <file.s> <name-substring> [--stats]
Prints the body of the first function whose label contains the substring
(until the next top-level function label), with Rust symbols shortened."""
import re, sys
path, needle = sys.argv[1], sys.argv[2]
stats = "--stats" in sys.argv
lines = open(path, encoding="utf-8", errors="replace").read().split("\n")
label = re.compile(r'^([^\s.#][^\s]*|"[^"]+"):\s*(#.*)?$')
def short(s):
    s = re.sub(r'_ZN(?:\d+[A-Za-z_$.][A-Za-z0-9_$.]*)+?17h[0-9a-f]{16}E', lambda m: demangle(m.group(0)), s)
    return s
def demangle(sym):
    body = sym[3:]; parts = []
    i = 0
    while body[i].isdigit():
        j = i
        while body[j].isdigit(): j += 1
        n = int(body[i:j]); parts.append(body[j:j+n]); i = j+n
    parts = [p for p in parts if not re.fullmatch(r'h[0-9a-f]{16}', p)]
    name = "::".join(parts)
    for a,b in [("$LT$","<"),("$GT$",">"),("$u20$"," "),("$u7b$","{"),("$u7d$","}"),("$C$",","),("..","::"),("$RF$","&"),("$BP$","*")]:
        name = name.replace(a,b)
    return name
start = None
for i,l in enumerate(lines):
    m = label.match(l)
    if m and needle in m.group(1) and not m.group(1).startswith(('"?dtor','$')):
        start = i; break
if start is None: sys.exit("not found")
out = [lines[start]]
for l in lines[start+1:]:
    m = label.match(l)
    if m and not m.group(1).startswith(('$', '"?dtor', '.')) and not re.match(r'^\.?LBB|^\$L', m.group(1)):
        break
    out.append(l)
body = [short(l) for l in out if l.strip() and not l.strip().startswith(('.seh_', '.cv_', '.p2align'))]
if stats:
    ins = [l for l in body if l.startswith('\t') and not l.strip().startswith('.')]
    calls = [l.split('call')[1].strip() for l in ins if l.strip().startswith('call')]
    print(f"{short(out[0])}\n  instructions: {len(ins)}   calls: {len(calls)}")
    from collections import Counter
    for c,n in Counter(calls).most_common(40): print(f"   {n:3} x {c}")
else:
    print("\n".join(body))
