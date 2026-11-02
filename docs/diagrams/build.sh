#!/bin/sh
# Build every diagram in this directory to SVG and PNG with TeX Live in Docker.
# Usage: docs/diagrams/build.sh [name ...]   (default: all *.tex except style.tex)
# Needs Docker; the first run builds the local image speeddemon-tex (see Dockerfile).
set -e
cd "$(dirname "$0")"
docker image inspect speeddemon-tex >/dev/null 2>&1 || docker build -t speeddemon-tex .
if [ $# -eq 0 ]; then set -- $(ls *.tex | grep -v '^style.tex$' | sed 's/\.tex$//'); fi
for n in "$@"; do
  MSYS_NO_PATHCONV=1 docker run --rm -v "$(pwd -W 2>/dev/null || pwd):/work" -w /work speeddemon-tex sh -c "
    pdflatex -interaction=nonstopmode -halt-on-error $n.tex >$n.log 2>&1 || { tail -30 $n.log; exit 1; }
    pdftocairo -svg $n.pdf $n.svg
    pdftocairo -png -r 160 -singlefile $n.pdf $n
    rm -f $n.aux $n.log $n.pdf"
  echo "built $n"
done
