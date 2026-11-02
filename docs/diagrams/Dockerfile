# TeX Live (small) plus the packages the diagrams need, and pdftocairo for SVG/PNG.
FROM ghcr.io/xu-cheng/texlive-small:latest
RUN tlmgr update --self >/dev/null 2>&1; tlmgr install standalone pgfplots xcolor pgf tools fontspec 2>&1 | tail -3 \
 && apk add --no-cache poppler-utils font-dejavu
