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

clean:
	cargo clean

.PHONY: all install uninstall test clean
