PREFIX ?= /usr/local
MAN_PREFIX ?= ${PREFIX}/man

all:
	cargo build --release

install:
	cargo build --release
	install -d ${PREFIX}/bin
	install -c -m 555 target/release/snare ${PREFIX}/bin/snare
	install -d ${MAN_PREFIX}/man/man1
	install -d ${MAN_PREFIX}/man/man5
	install -c -m 444 snare.1 ${MAN_PREFIX}/man/man1/snare.1
	install -c -m 444 snare.conf.5 ${MAN_PREFIX}/man/man5/snare.conf.5
	install -d ${PREFIX}/share/examples/snare
	install -c -m 444 snare.conf.example ${PREFIX}/share/examples/snare

distrib:
	test "X`git status --porcelain`" = "X"
	@read v?'snare version: ' && mkdir snare-$$v && \
      cp -rp Cargo.lock Cargo.toml COPYRIGHT LICENSE-APACHE LICENSE-MIT \
	    Makefile README.md build.rs snare.1 snare.conf.5 snare.conf.example \
		src snare-$$v && \
	  tar cfz snare-$$v.tgz snare-$$v && rm -rf snare-$$v
