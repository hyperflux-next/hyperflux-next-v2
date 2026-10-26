# SPDX-License-Identifier: GPL-2.0-only
#
# hyperfluxctl's man page and shell completions, and syntax checks for
# them. The top-level Makefile defines the paths used here.

CLI_COMPLETIONS := crates/hyperfluxctl/completions

.PHONY: install-man install-completions cli-test

install-man:
	install -Dm0644 man/hyperfluxctl.1 $(DESTDIR)$(MANDIR)/man1/hyperfluxctl.1

install-completions:
	install -Dm0644 $(CLI_COMPLETIONS)/hyperfluxctl.bash $(DESTDIR)$(BASHCOMPDIR)/hyperfluxctl
	install -Dm0644 $(CLI_COMPLETIONS)/_hyperfluxctl $(DESTDIR)$(ZSHCOMPDIR)/_hyperfluxctl
	install -Dm0644 $(CLI_COMPLETIONS)/hyperfluxctl.fish $(DESTDIR)$(FISHCOMPDIR)/hyperfluxctl.fish

install-linux: install-man install-completions

# Each check runs where its tool is installed, and bash always is.
cli-test:
	bash -n $(CLI_COMPLETIONS)/hyperfluxctl.bash
	if command -v zsh >/dev/null; then zsh -n $(CLI_COMPLETIONS)/_hyperfluxctl; fi
	if command -v fish >/dev/null; then fish -n $(CLI_COMPLETIONS)/hyperfluxctl.fish; fi
	if command -v groff >/dev/null; then \
		test -z "$$(groff -man -ww -z man/hyperfluxctl.1 2>&1)"; fi

TEST_TARGETS += cli-test
