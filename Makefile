PREFIX ?= $(HOME)/.local
BIN = target/release/subs
export RUSTFLAGS

all: $(BIN)

$(BIN): Makefile Cargo.toml Cargo.lock $(wildcard src/*.rs src/*.py src/prov/*.rs)
	@set -f; \
	separator=$$(printf '\037'); \
	encoded="$${CARGO_ENCODED_RUSTFLAGS:-}"; \
	if [ -z "$$encoded" ]; then \
	  for flag in $${RUSTFLAGS:-}; do encoded="$$encoded$${encoded:+$$separator}$$flag"; done; \
	fi; \
	encoded="$$encoded$${encoded:+$$separator}--remap-path-prefix=$$HOME=/build/user$${separator}--remap-path-prefix=$$(pwd -P)=/build/subs"; \
	CARGO_ENCODED_RUSTFLAGS="$$encoded" cargo build --release --locked

install: $(BIN)
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 $(BIN) $(DESTDIR)$(PREFIX)/bin/subs

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/subs

test:
	cargo test --locked

# README recordings; needs vhs (github.com/charmbracelet/vhs). Demo data only.
gifs: $(BIN)
	for t in status tui use; do vhs docs/$$t.tape; done

clean:
	cargo clean

.PHONY: all install uninstall test clean gifs
