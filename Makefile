PREFIX ?= $(HOME)/.local

install:
	install -d $(DESTDIR)$(PREFIX)/bin
	install -m 755 subs $(DESTDIR)$(PREFIX)/bin/subs

uninstall:
	rm -f $(DESTDIR)$(PREFIX)/bin/subs

.PHONY: install uninstall
