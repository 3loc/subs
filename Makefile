PREFIX ?= $(HOME)/.local
BIN = target/release/subs

all: $(BIN)

$(BIN): Cargo.toml Cargo.lock $(wildcard src/*.rs)
	cargo build --release --locked

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
