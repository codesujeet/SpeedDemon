#!/bin/bash
# usage: cgpeak.sh cpus bin  -> prints peak MiB (cgroup v1 max_usage_in_bytes)
G=/sys/fs/cgroup/memory/sdtest; mkdir -p $G
echo 0 > $G/memory.max_usage_in_bytes
bash -c "echo \$\$ > $G/cgroup.procs; exec taskset -c $1 $2"
awk '{printf "%.2f MiB\n", $1/1048576}' $G/memory.max_usage_in_bytes
