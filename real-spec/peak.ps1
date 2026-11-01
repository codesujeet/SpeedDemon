# Peak working set of one track run on full data (a rough local proxy for
# the harness's cgroup memory.peak), plus wall time.
param([string]$Track, [string]$Scale = "full")
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$dirs = @{ a = "a_ingest"; b = "b_index"; c = "c_join"; d = "d_traverse" }
$data = Join-Path $root "data\$($dirs[$Track])\$Scale"
$out = Join-Path $root "rust-out\$Track\$Scale\peak-out.txt"
switch ($Track) {
    "a" { $env:INPUT_PATH = "$data\requests.log" }
    "b" { $env:DOCS_PATH = "$data\docs.txt"; $env:QUERIES_PATH = "$data\queries.txt" }
    "c" { $env:RECORDS_PATH = "$data\records.csv"; $env:REGIONS_PATH = "$data\regions.csv" }
    "d" { $env:GRAPH_PATH = "$data\graph.txt"; $env:QUERIES_PATH = "$data\queries.txt" }
}
$env:OUTPUT_PATH = $out
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath (Join-Path $root "bin\track-$Track.exe") -PassThru -WindowStyle Hidden -RedirectStandardError "NUL"
$peak = 0
while (-not $p.HasExited) {
    try { $p.Refresh(); if ($p.PeakWorkingSet64 -gt $peak) { $peak = $p.PeakWorkingSet64 } } catch {}
    Start-Sleep -Milliseconds 5
}
"{0} {1}: {2} ms, peak {3:N0} MB" -f $Track, $Scale, $sw.ElapsedMilliseconds, ($peak / 1MB)
Remove-Item Env:\INPUT_PATH, Env:\DOCS_PATH, Env:\QUERIES_PATH, Env:\RECORDS_PATH, Env:\REGIONS_PATH, Env:\GRAPH_PATH, Env:\OUTPUT_PATH -ErrorAction SilentlyContinue
