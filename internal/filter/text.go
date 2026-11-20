package filter

import (
	"fmt"
	"strconv"
	"strings"
)

// ParseText parses a filter in the text form and validates it against the
// collection's fields. The empty string is no filter: the result is nil, which
// matches everything.
//
// The form is documented on the package: comparisons as "field op value", where
// op is = == != <> < <= > >= in contains prefix eq ne lt le gt ge, combined with
// and, or and not and parentheses, with true, false, null, quoted strings and
// [ … ] lists. A bare word is a string. "field is null" and "field is not null"
// are the null tests.
func ParseText(text string, fields Fields) (*Filter, error) {
	parsed, err := parseText(text)
	if err != nil {
		return nil, err
	}
	if err := parsed.Validate(fields); err != nil {
		return nil, err
	}
	return parsed, nil
}

// EncodeText converts a textual filter to its wire form. The server validates
// its fields and operands against the collection that receives it.
func EncodeText(text string) ([]byte, error) {
	parsed, err := parseText(text)
	if err != nil {
		return nil, err
	}
	return Marshal(parsed)
}

func parseText(text string) (*Filter, error) {
	if strings.TrimSpace(text) == "" {
		return nil, nil
	}

	tokens, err := scan(text)
	if err != nil {
		return nil, err
	}
	parser := &parser{tokens: tokens}
	parsed, err := parser.parseOr()
	if err != nil {
		return nil, err
	}
	if token := parser.peek(); token.kind != tokEOF {
		return nil, fmt.Errorf("%w: unexpected %s at offset %d", ErrInvalid, token.describe(), token.pos)
	}
	return parsed, nil
}

// tokenKind is what a token of the text form is.
type tokenKind int

const (
	tokEOF tokenKind = iota
	tokWord
	tokNumber
	tokString
	tokOperator
	tokLParen
	tokRParen
	tokLBracket
	tokRBracket
	tokComma
)

// token is one piece of the text form.
type token struct {
	kind tokenKind
	text string
	pos  int
}

// describe names a token in an error.
func (t token) describe() string {
	switch t.kind {
	case tokEOF:
		return "the end"
	case tokString:
		return "a string"
	case tokNumber:
		return "the number " + t.text
	case tokOperator:
		return fmt.Sprintf("%q", t.text)
	case tokLParen:
		return `"("`
	case tokRParen:
		return `")"`
	case tokLBracket:
		return `"["`
	case tokRBracket:
		return `"]"`
	case tokComma:
		return `","`
	default:
		return fmt.Sprintf("%q", t.text)
	}
}

// scan splits the text form into tokens.
func scan(text string) ([]token, error) {
	tokens := make([]token, 0, 16)
	for at := 0; at < len(text); {
		char := text[at]
		switch {
		case char == ' ' || char == '\t' || char == '\n' || char == '\r':
			at++
		case char == '(':
			tokens = append(tokens, token{kind: tokLParen, text: "(", pos: at})
			at++
		case char == ')':
			tokens = append(tokens, token{kind: tokRParen, text: ")", pos: at})
			at++
		case char == '[':
			tokens = append(tokens, token{kind: tokLBracket, text: "[", pos: at})
			at++
		case char == ']':
			tokens = append(tokens, token{kind: tokRBracket, text: "]", pos: at})
			at++
		case char == ',':
			tokens = append(tokens, token{kind: tokComma, text: ",", pos: at})
			at++
		case char == '"' || char == '\'':
			value, next, err := scanString(text, at)
			if err != nil {
				return nil, err
			}
			tokens = append(tokens, token{kind: tokString, text: value, pos: at})
			at = next
		case strings.IndexByte("=!<>", char) >= 0:
			operator, next := scanOperator(text, at)
			tokens = append(tokens, token{kind: tokOperator, text: operator, pos: at})
			at = next
		case isDigit(char) || (char == '-' || char == '+') && at+1 < len(text) && isDigit(text[at+1]):
			number, next := scanNumber(text, at)
			tokens = append(tokens, token{kind: tokNumber, text: number, pos: at})
			at = next
		default:
			word, next := scanWord(text, at)
			if word == "" {
				return nil, fmt.Errorf("%w: %q at offset %d is not part of a filter", ErrInvalid, char, at)
			}
			tokens = append(tokens, token{kind: tokWord, text: word, pos: at})
			at = next
		}
	}
	return append(tokens, token{kind: tokEOF, pos: len(text)}), nil
}

// scanString reads a quoted string, unescaping \n, \t, \r, \\ and the quote.
func scanString(text string, at int) (string, int, error) {
	quote := text[at]
	var builder strings.Builder
	for i := at + 1; i < len(text); i++ {
		switch char := text[i]; char {
		case quote:
			return builder.String(), i + 1, nil
		case '\\':
			if i+1 >= len(text) {
				return "", 0, fmt.Errorf("%w: a string ends with a backslash at offset %d", ErrInvalid, at)
			}
			i++
			switch escaped := text[i]; escaped {
			case 'n':
				builder.WriteByte('\n')
			case 't':
				builder.WriteByte('\t')
			case 'r':
				builder.WriteByte('\r')
			case '\\', '"', '\'':
				builder.WriteByte(escaped)
			default:
				builder.WriteByte('\\')
				builder.WriteByte(escaped)
			}
		default:
			builder.WriteByte(char)
		}
	}
	return "", 0, fmt.Errorf("%w: the string at offset %d is not closed", ErrInvalid, at)
}

// scanOperator reads an operator.
func scanOperator(text string, at int) (string, int) {
	if at+1 < len(text) {
		switch pair := text[at : at+2]; pair {
		case "==", "!=", "<>", "<=", ">=":
			return pair, at + 2
		}
	}
	return text[at : at+1], at + 1
}

// scanNumber reads a number.
func scanNumber(text string, at int) (string, int) {
	i := at
	if text[i] == '-' || text[i] == '+' {
		i++
	}
	for i < len(text) && (isDigit(text[i]) || text[i] == '.' || text[i] == 'e' || text[i] == 'E' ||
		(text[i] == '-' || text[i] == '+') && i > at && (text[i-1] == 'e' || text[i-1] == 'E')) {
		i++
	}
	return text[at:i], i
}

// scanWord reads a bare word: a field name, a keyword, or a value such as a
// resource URI.
func scanWord(text string, at int) (string, int) {
	i := at
	for i < len(text) {
		char := text[i]
		switch char {
		case ' ', '\t', '\n', '\r', '(', ')', '[', ']', ',', '"', '\'', '=', '!', '<', '>':
			return text[at:i], i
		default:
			i++
		}
	}
	return text[at:i], i
}

func isDigit(char byte) bool { return char >= '0' && char <= '9' }

// parser reads the tokens of the text form.
type parser struct {
	tokens []token
	at     int
}

// peek is the next token.
func (p *parser) peek() token { return p.tokens[p.at] }

// take returns the next token.
func (p *parser) take() token {
	token := p.tokens[p.at]
	if token.kind != tokEOF {
		p.at++
	}
	return token
}

// parseOr reads a filter of ands joined by or.
func (p *parser) parseOr() (*Filter, error) {
	first, err := p.parseAnd()
	if err != nil {
		return nil, err
	}
	children := []*Filter{first}
	for p.isWord("or") {
		p.take()
		child, err := p.parseAnd()
		if err != nil {
			return nil, err
		}
		children = append(children, child)
	}
	if len(children) == 1 {
		return first, nil
	}
	return Or(children...), nil
}

// parseAnd reads filters joined by and.
func (p *parser) parseAnd() (*Filter, error) {
	first, err := p.parseUnary()
	if err != nil {
		return nil, err
	}
	children := []*Filter{first}
	for p.isWord("and") {
		p.take()
		child, err := p.parseUnary()
		if err != nil {
			return nil, err
		}
		children = append(children, child)
	}
	if len(children) == 1 {
		return first, nil
	}
	return And(children...), nil
}

// parseUnary reads not, a parenthesised filter, or a comparison.
func (p *parser) parseUnary() (*Filter, error) {
	if p.isWord("not") {
		p.take()
		child, err := p.parseUnary()
		if err != nil {
			return nil, err
		}
		return Not(child), nil
	}
	if token := p.peek(); token.kind == tokLParen {
		p.take()
		inner, err := p.parseOr()
		if err != nil {
			return nil, err
		}
		if closing := p.take(); closing.kind != tokRParen {
			return nil, fmt.Errorf("%w: expected ) at offset %d and found %s", ErrInvalid, closing.pos, closing.describe())
		}
		return inner, nil
	}
	return p.parseComparison()
}

// parseComparison reads a comparison of a field with a value.
func (p *parser) parseComparison() (*Filter, error) {
	field := p.take()
	if field.kind != tokWord {
		return nil, fmt.Errorf("%w: expected a field at offset %d and found %s", ErrInvalid, field.pos, field.describe())
	}

	next := p.take()
	switch next.kind {
	case tokOperator:
		op, err := ParseOp(next.text)
		if err != nil {
			return nil, err
		}
		value, err := p.parseValue()
		if err != nil {
			return nil, err
		}
		return Compare(op, field.text, value), nil
	case tokWord:
		switch strings.ToLower(next.text) {
		case "in":
			values, err := p.parseList()
			if err != nil {
				return nil, err
			}
			return In(field.text, values...), nil
		case "contains", "prefix", "eq", "ne", "lt", "le", "gt", "ge":
			op, _ := ParseOp(next.text)
			value, err := p.parseValue()
			if err != nil {
				return nil, err
			}
			return Compare(op, field.text, value), nil
		case "is":
			return p.parseNullTest(field.text)
		case "and", "or":
			// A bool field stands alone: "done and …" is "done = true and …".
			p.at--
			return Compare(OpEq, field.text, true), nil
		default:
			// A value with no operator is equality: "state open".
			p.at--
			value, err := p.parseValue()
			if err != nil {
				return nil, err
			}
			return Compare(OpEq, field.text, value), nil
		}
	case tokNumber, tokString, tokLBracket:
		// A value with no operator is equality.
		p.at--
		value, err := p.parseValue()
		if err != nil {
			return nil, err
		}
		return Compare(OpEq, field.text, value), nil
	case tokEOF:
		// A bool field stands alone: "done" is "done = true".
		return Compare(OpEq, field.text, true), nil
	case tokRParen, tokRBracket, tokComma:
		// A bool field stands alone too: "(done)" is "(done = true)".
		p.at--
		return Compare(OpEq, field.text, true), nil
	default:
		return nil, fmt.Errorf("%w: expected an operator after %q at offset %d and found %s",
			ErrInvalid, field.text, next.pos, next.describe())
	}
}

// parseNullTest reads "is null" and "is not null".
func (p *parser) parseNullTest(field string) (*Filter, error) {
	next := p.take()
	if next.kind != tokWord {
		return nil, fmt.Errorf("%w: expected null or not after is at offset %d and found %s",
			ErrInvalid, next.pos, next.describe())
	}
	switch strings.ToLower(next.text) {
	case "null":
		return Compare(OpEq, field, nil), nil
	case "not":
		if null := p.take(); null.kind != tokWord || !strings.EqualFold(null.text, "null") {
			return nil, fmt.Errorf("%w: expected null after is not at offset %d and found %s",
				ErrInvalid, null.pos, null.describe())
		}
		return Compare(OpNe, field, nil), nil
	default:
		return nil, fmt.Errorf("%w: expected null or not after is at offset %d and found %s",
			ErrInvalid, next.pos, next.describe())
	}
}

// parseList reads a [ … ] list of values.
func (p *parser) parseList() ([]any, error) {
	open := p.take()
	if open.kind != tokLBracket {
		return nil, fmt.Errorf("%w: expected [ at offset %d and found %s", ErrInvalid, open.pos, open.describe())
	}
	values := make([]any, 0, 4)
	for {
		if closing := p.peek(); closing.kind == tokRBracket {
			p.take()
			return values, nil
		}
		value, err := p.parseValue()
		if err != nil {
			return nil, err
		}
		values = append(values, value)

		next := p.take()
		switch next.kind {
		case tokComma:
		case tokRBracket:
			return values, nil
		default:
			return nil, fmt.Errorf("%w: expected , or ] at offset %d and found %s", ErrInvalid, next.pos, next.describe())
		}
	}
}

// parseValue reads a value.
func (p *parser) parseValue() (any, error) {
	token := p.take()
	switch token.kind {
	case tokString:
		return token.text, nil
	case tokNumber:
		return parseNumber(token.text)
	case tokLBracket:
		p.at--
		return p.parseList()
	case tokWord:
		switch strings.ToLower(token.text) {
		case "true":
			return true, nil
		case "false":
			return false, nil
		case "null":
			return nil, nil
		default:
			// A bare word is a string, so an enum or a resource URI needs no
			// quotes.
			return token.text, nil
		}
	default:
		return nil, fmt.Errorf("%w: expected a value at offset %d and found %s", ErrInvalid, token.pos, token.describe())
	}
}

// parseNumber reads an integer or a float, whichever the text is.
func parseNumber(text string) (any, error) {
	if !strings.ContainsAny(text, ".eE") {
		number, err := strconv.ParseInt(text, 10, 64)
		if err == nil {
			return number, nil
		}
	}
	number, err := strconv.ParseFloat(text, 64)
	if err != nil {
		return nil, fmt.Errorf("%w: %q is not a number", ErrInvalid, text)
	}
	return number, nil
}

// isWord reports whether the next token is that word.
func (p *parser) isWord(word string) bool {
	token := p.peek()
	return token.kind == tokWord && strings.EqualFold(token.text, word)
}
