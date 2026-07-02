# Build the Seal technical reference (PDF + EPUB) from the LaTeX source.
#
#   make pdf     -> seal.pdf   (via tectonic)
#   make epub    -> seal.epub  (via pandoc)
#   make all     -> both
#   make clean   -> remove build artifacts
#
# Requires: tectonic and pandoc (brew install tectonic pandoc).

SRC     := latex/seal.tex
PDF     := seal.pdf
EPUB    := seal.epub
TITLE   := Seal: A Technical Reference
AUTHOR  := Seal Project

.PHONY: all pdf epub clean

all: pdf epub

pdf: $(PDF)

$(PDF): $(SRC) $(wildcard latex/chapters/*.tex)
	tectonic $(SRC) --outdir .

epub: $(EPUB)

# pandoc resolves \include relative to its working directory, so build from
# inside latex/ and write the artifact back up to docs/.
$(EPUB): $(SRC) $(wildcard latex/chapters/*.tex)
	cd latex && pandoc seal.tex -o ../$(EPUB) \
		--toc --toc-depth=2 \
		--metadata title="$(TITLE)" \
		--metadata author="$(AUTHOR)"

clean:
	rm -f $(PDF) $(EPUB)
