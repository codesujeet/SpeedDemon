#!/bin/bash
# usage: g15build.sh name [extra g++ flags]   compiles $S/g15/name.cpp with the board's gcc 15 into $S/g15/name
S=/tmp/claude-0/-home-user-SpeedDemon/41f9d852-ae71-53d3-97e9-45f8193ee723/scratchpad
if ! docker info >/dev/null 2>&1; then
  pkill dockerd 2>/dev/null; pkill containerd 2>/dev/null; sleep 1; mkdir -p /run/dk
  (nohup dockerd --data-root $S/docker/data --exec-root /run/dk --pidfile /run/dk/pid -H unix:///var/run/docker.sock > $S/docker/dockerd.log 2>&1 &)
  for i in $(seq 1 30); do sleep 2; docker info >/dev/null 2>&1 && break; done
fi
IMG=$(grep -m1 "^FROM gcc" /home/user/SpeedDemon/track-a/Dockerfile | awk '{print $2}')
docker run --rm --network=host -v $S/g15:/w2 $IMG g++ -O3 -march=x86-64-v3 -mpclmul -mbmi2 -std=c++20 -pthread -static "${@:2}" -o /w2/$1 /w2/$1.cpp 2>&1 | head -20
