package filter

import (
	"fmt"
	"strconv"
	"time"

	"github.com/jackc/pgx/v5"
)

// SQL returns the filter as a boolean SQL condition and the arguments for it. A
// nil filter is the condition "true" with no arguments, so a query can always
// place it in a WHERE.
//
// Placeholders are numbered from first, so a query that puts the condition
// before anything else, or after a few arguments of its own, decides where its
// arguments go. Column names are the declared fields' names, quoted as
// identifiers. Every value is a parameter: no value is ever part of the
// condition's text.
//
// The filter must be validated first.
func (f *Filter) SQL(first int) (string, []any, error) {
	if f == nil {
		return "true", nil, nil
	}
	if !f.validated {
		return "", nil, ErrUnvalidated
	}
	if first < 1 {
		return "", nil, fmt.Errorf("%w: the first parameter is numbered from 1", ErrInvalid)
	}

	args := &sqlArgs{next: first}
	clause, err := f.compile(args)
	if err != nil {
		return "", nil, err
	}
	return clause, args.values, nil
}

// sqlArgs numbers a condition's placeholders and collects its values.
type sqlArgs struct {
	values []any
	next   int
}

// add puts a value in the arguments and returns its placeholder.
func (a *sqlArgs) add(value any) string {
	a.values = append(a.values, value)
	placeholder := "$" + strconv.Itoa(a.next)
	a.next++
	return placeholder
}

// sliceOf is a validated in-list as a slice of the field's Go type, which is
// how pgx encodes an array for a typed column. A plain []any would lose the
// type and the column's array type could not be inferred.
func sliceOf(typ Type, values []any) any {
	switch typ {
	case TypeInt:
		numbers := make([]int64, len(values))
		for i, value := range values {
			numbers[i], _ = value.(int64)
		}
		return numbers
	case TypeFloat:
		numbers := make([]float64, len(values))
		for i, value := range values {
			numbers[i], _ = value.(float64)
		}
		return numbers
	case TypeBool:
		flags := make([]bool, len(values))
		for i, value := range values {
			flags[i], _ = value.(bool)
		}
		return flags
	case TypeTime:
		moments := make([]time.Time, len(values))
		for i, value := range values {
			moments[i], _ = value.(time.Time)
		}
		return moments
	case TypeCBOR:
		blobs := make([][]byte, len(values))
		for i, value := range values {
			blobs[i], _ = value.([]byte)
		}
		return blobs
	default:
		texts := make([]string, len(values))
		for i, value := range values {
			texts[i], _ = value.(string)
		}
		return texts
	}
}

// compile renders a filter as a condition.
func (f *Filter) compile(args *sqlArgs) (string, error) {
	switch f.op {
	case OpAnd, OpOr:
		if len(f.children) == 0 {
			if f.op == OpAnd {
				return "true", nil
			}
			return "false", nil
		}
		joiner := " AND "
		if f.op == OpOr {
			joiner = " OR "
		}
		condition := ""
		for i, child := range f.children {
			part, err := child.compile(args)
			if err != nil {
				return "", err
			}
			if i > 0 {
				condition += joiner
			}
			if len(f.children) > 1 {
				part = "(" + part + ")"
			}
			condition += part
		}
		return condition, nil
	case OpNot:
		part, err := f.children[0].compile(args)
		if err != nil {
			return "", err
		}
		return "NOT (" + part + ")", nil
	}

	column := pgx.Identifier{f.field}.Sanitize()

	switch f.op {
	case OpEq, OpNe:
		if f.value == nil {
			if f.op == OpEq {
				return column + " IS NULL", nil
			}
			return column + " IS NOT NULL", nil
		}
		operator := "="
		if f.op == OpNe {
			operator = "<>"
		}
		return column + " " + operator + " " + args.add(f.value), nil
	case OpIn:
		return column + " = ANY(" + args.add(sliceOf(f.fieldType, f.values)) + ")", nil
	case OpLt, OpLe, OpGt, OpGe:
		operators := map[Op]string{OpLt: "<", OpLe: "<=", OpGt: ">", OpGe: ">="}
		operand := column
		if f.fieldType == TypeString || f.fieldType == TypeText {
			// Order strings by their bytes, which is what the matcher does and
			// what the database's collation may not.
			operand = "(" + column + ` COLLATE "C")`
		}
		return operand + " " + operators[f.op] + " " + args.add(f.value), nil
	case OpContains:
		return "strpos(" + column + ", " + args.add(f.value) + ") > 0", nil
	case OpPrefix:
		return "starts_with(" + column + ", " + args.add(f.value) + ")", nil
	default:
		return "", fmt.Errorf("%w: %q is not an operator", ErrInvalid, f.op)
	}
}
