import subprocess, sys
for f in sys.argv[1:]:
    src = open(f).read()
    fmt = subprocess.run(["clang-format", "--style=LLVM", "--assume-filename=main.cpp"], input=src, capture_output=True, text=True).stdout
    n = 0; blk = False
    for l in fmt.split("\n"):
        s = l.strip()
        if blk:
            blk = "*/" not in s; continue
        if not s or s.startswith("//"): continue
        if s.startswith("/*"): blk = "*/" not in s; continue
        n += 1
    print(f"{f.split('/')[-1]}: {n} formatted code lines")
