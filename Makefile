PREFIX ?= /usr/local
BINDIR ?= ${PREFIX}/bin
LIBDIR ?= ${PREFIX}/lib
SHAREDIR ?= ${PREFIX}/share
EXAMPLESDIR ?= ${SHAREDIR}/examples

MANDIR.${PREFIX} = ${PREFIX}/share/man
MANDIR./usr/local = /usr/local/man
MANDIR. = /usr/share/man
MANDIR ?= ${MANDIR.${PREFIX}}

.PHONY: all install distrib

all:
	cargo build --release

install:
	cargo build --release
	install -d ${DESTDIR}${BINDIR}
	install -c -m 555 target/release/snare ${DESTDIR}${BINDIR}/snare
	install -d ${DESTDIR}${MANDIR}/man1
	install -d ${DESTDIR}${MANDIR}/man5
	install -c -m 444 snare.1 ${DESTDIR}${MANDIR}/man1/snare.1
	install -c -m 444 snare.conf.5 ${DESTDIR}${MANDIR}/man5/snare.conf.5
	install -d ${DESTDIR}${EXAMPLESDIR}/pizauth
	install -c -m 444 snare.conf.example ${DESTDIR}${EXAMPLESDIR}/snare

distrib:
	test "X`git status --porcelain`" = "X"
	@read v?'snare version: ' \
	  && mkdir snare-$$v \
	  && cp -rp Cargo.lock Cargo.toml COPYRIGHT LICENSE-APACHE LICENSE-MIT \
	    Makefile CHANGES.md README.md build.rs snare.1 snare.conf.5 \
	    snare.conf.example src tests snare-$$v \
	  && tar cfz snare-$$v.tgz snare-$$v \
	  && rm -rf snare-$$v
