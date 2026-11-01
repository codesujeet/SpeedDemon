import subprocess, sys
for f in sys.argv[1:]:
    fmt = subprocess.run(["rustfmt", "--edition", "2021", "--emit", "stdout", "--config", "max_width=100", f], capture_output=True, text=True).stdout
    n = 0
    for l in fmt.split("\n"):
        s = l.strip()
        if s and not s.startswith("//") and not s.startswith("///") and not s.startswith("//!") and not s.startswith("Checking") and not s.startswith(f): n += 1
    print(f"{f.split('/')[-3] if f.count('/')>2 else f}: ~{n} formatted code lines (rustfmt default config)")
