package main

import (
	"bytes"
	"fmt"
	"path/filepath"
	"strings"

	"mvdan.cc/sh/v3/fileutil"
	"mvdan.cc/sh/v3/syntax"
)

// formatOptions are the settings poly passes to its shell formatter: the
// same `-i <width>` (0 for tabs) the host tool receives.
type formatOptions struct {
	indent uint
}

// dialectFor names the shell dialect shfmt picks with no -ln flag: a shell
// config file name or a file extension wins, then the shebang, and bash is the
// fallback. The name is the one LangVariant.Set accepts, and it is also what
// poly's host folds into the format cache key (Rust: host_hooks::shell_dialect),
// which testdata/dialects.json pins on both sides.
func dialectFor(path string, src []byte) string {
	switch strings.TrimPrefix(filepath.Base(path), ".") {
	case "bash_profile", "bashrc", "bash_logout":
		return "bash"
	case "zshenv", "zprofile", "zshrc", "zlogin", "zlogout":
		return "zsh"
	}
	var lang syntax.LangVariant
	if ext := strings.TrimPrefix(filepath.Ext(path), "."); ext != "sh" && lang.Set(ext) == nil && lang != syntax.LangAuto {
		return canonicalDialect(lang)
	}
	if err := lang.Set(fileutil.Shebang(src)); err != nil || lang == syntax.LangAuto {
		return "bash"
	}
	return canonicalDialect(lang)
}

// canonicalDialect is the single name of a resolved variant, so the aliases
// sh, dash and posix are one dialect.
func canonicalDialect(lang syntax.LangVariant) string {
	switch lang {
	case syntax.LangPOSIX:
		return "posix"
	case syntax.LangMirBSDKorn:
		return "mksh"
	case syntax.LangBats:
		return "bats"
	case syntax.LangZsh:
		return "zsh"
	default:
		return "bash"
	}
}

// variantFor is the parser variant for dialectFor.
func variantFor(path string, src []byte) syntax.LangVariant {
	var lang syntax.LangVariant
	if err := lang.Set(dialectFor(path, src)); err != nil {
		return syntax.LangBash
	}
	return lang
}

// formatSource formats shell source. It returns changed=false when the source
// is already formatted. A source that does not parse is an error, which the
// caller reports as "leave the file alone", as the host tool's non-zero exit
// does.
func formatSource(path string, src []byte, opts formatOptions) (formatted string, changed bool, err error) {
	parser := syntax.NewParser(syntax.KeepComments(true), syntax.Variant(variantFor(path, src)))
	file, err := parser.Parse(bytes.NewReader(src), path)
	if err != nil {
		return "", false, fmt.Errorf("parse %s: %w", path, err)
	}
	printer := syntax.NewPrinter(syntax.Indent(opts.indent))
	var out bytes.Buffer
	if err := printer.Print(&out, file); err != nil {
		return "", false, fmt.Errorf("print %s: %w", path, err)
	}
	if bytes.Equal(out.Bytes(), src) {
		return "", false, nil
	}
	return out.String(), true, nil
}
