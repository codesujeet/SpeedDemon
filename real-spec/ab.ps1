# Alternates two prebuilt images ($TagA, $TagB) for N rounds on one track and
# prints median wall ms and median peak MB for each. Interleaving cancels
# slow drift on this noisy machine; medians cancel outliers.
param([Parameter(Mandatory)][string]$Track, [string]$Scale = "full", [int]$Rounds = 5,
      [string]$TagA = "read", [string]$TagB = "mmap")
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$res = @{ $TagA = @(); $TagB = @() }
for ($i = 0; $i -lt $Rounds; $i++) {
    foreach ($tag in @($TagA, $TagB)) {
        $line = & (Join-Path $root "dock.ps1") -Track $Track -Scale $Scale -Image "sd-track-$Track-$tag" -Runs 1
        $m = [regex]::Match("$line", "(\d+) ms\s+peak\s+([\d,]+) MB.*?(MATCH|MISMATCH)")
        if ($m.Groups[3].Value -ne "MATCH") { Write-Warning "$tag run $i : $line" }
        $res[$tag] += , @([double]$m.Groups[1].Value, [double]($m.Groups[2].Value -replace ",", ""))
    }
}
function Median($a) { $s = $a | Sort-Object; $s[[int][math]::Floor(($s.Count - 1) / 2)] }
foreach ($tag in @($TagA, $TagB)) {
    $ms = $res[$tag] | ForEach-Object { $_[0] }
    $mb = $res[$tag] | ForEach-Object { $_[1] }
    "{0} {1,-6} median {2,6} ms (min {3}, all {4})  peak {5} MB" -f $Track, $tag, (Median $ms), ($ms | Measure-Object -Minimum).Minimum, (($ms | ForEach-Object { [int]$_ }) -join ","), (Median $mb)
}
