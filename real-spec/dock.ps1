# Linux-faithful run of one track: builds the track's own Dockerfile, puts the
# dataset on a docker volume (ext4 in the VM, like the host's disk) and
# pre-warms it from a separate container (like the harness's vmtouch), then
# runs with the board's flags and reads the container cgroup's memory.peak
# from inside, which is the counter the harness reads. Prints wall ms
# (exec to exit, measured inside the container), peak MB and the output hash.
param(
    [Parameter(Mandatory)][string]$Track,
    [string]$Scale = "full",
    [int]$Runs = 1,
    [switch]$NoBuild,
    [string]$Image = "",
    [switch]$Timing
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path -Parent $root
$dirs = @{ a = "a_ingest"; b = "b_index"; c = "c_join"; d = "d_traverse" }
$outName = @{ a = "summary.txt"; b = "results.txt"; c = "aggregates.txt"; d = "distances.txt" }
$published = @{
    a = "495ca09682b9673ef836a506496177680bb4a3af54255174befddb1f83481cd3"
    b = "f86973c210228141bcb6f1f1b10188ad2a9b65f71e7cd7cf4590b38de4e80927"
    c = "d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536"
    d = "a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e"
}
$expected = if ($Scale -eq "full") { $published[$Track] } else {
    (Get-FileHash (Join-Path $root "out\$Track\$Scale\$($outName[$Track])") -Algorithm SHA256).Hash.ToLower()
}

$img = if ($Image) { $Image } else { "sd-track-$Track" }
if ($Image) { $NoBuild = $true }
if (-not $NoBuild) {
    docker build -q --network=none -t $img (Join-Path $repo "track-$Track") | Out-Null
    if ($LASTEXITCODE) { throw "build failed" }
}

$vol = "sd-data-$($dirs[$Track])-$Scale"
$ErrorActionPreference = "Continue"
$exists = (docker volume ls -q --filter "name=^${vol}$") -eq $vol
if (-not $exists) {
    docker volume create $vol | Out-Null
    $src = (Resolve-Path (Join-Path $root "data\$($dirs[$Track])\$Scale")).Path
    docker run --rm -v "${vol}:/dst" -v "${src}:/src:ro" debian:13-slim sh -c "cp -r /src/. /dst/" | Out-Null
}
$outVol = "sd-out-$Track"
docker volume rm -f $outVol | Out-Null
docker volume create $outVol | Out-Null
docker run --rm -v "${outVol}:/out" debian:13-slim chmod 777 /out | Out-Null

for ($i = 1; $i -le $Runs; $i++) {
    docker run --rm -v "${vol}:/data:ro" debian:13-slim sh -c "cat /data/* > /dev/null" | Out-Null
    $inner = 'S=$(date +%s%N); /app/run; C=$?; E=$(date +%s%N); echo WALL_MS=$(( (E-S)/1000000 )) EXIT=$C PEAK=$(cat /sys/fs/cgroup/memory.peak)'
    $timingEnv = if ($Timing) { @("-e", "TRACK_$($Track.ToUpper())_TIMING=1") } else { @() }
    $all = docker run --rm --network=none --read-only --tmpfs /tmp:rw,size=1g,exec --memory=8g --memory-swap=8g `
        --pids-limit=512 --cpuset-cpus=0-11 --cap-drop=ALL --security-opt=no-new-privileges --user 65534:65534 `
        --ulimit nofile=8192:8192 -e "TRACK=$Track" @timingEnv -v "${vol}:/data:ro" -v "${outVol}:/out" `
        --entrypoint sh $img -c $inner 2>&1 | ForEach-Object { "$_" }
    if ($Timing) { $all | Select-String "^track-" | ForEach-Object { "    $_" } }
    $line = $all | Select-String "WALL_MS"
    $m = [regex]::Match("$line", "WALL_MS=(\d+) EXIT=(\d+) PEAK=(\d+)")
    $hash = (docker run --rm -v "${outVol}:/out:ro" debian:13-slim sh -c "sha256sum /out/*" 2>$null) -split " " | Select-Object -First 1
    $ok = if ($hash -eq $expected) { "MATCH" } else { "MISMATCH" }
    "{0} {1} run {2}: {3} ms  peak {4:N0} MB  exit {5}  {6}" -f $Track, $Scale, $i, $m.Groups[1].Value, ([double]$m.Groups[3].Value / 1MB), $m.Groups[2].Value, $ok
}
