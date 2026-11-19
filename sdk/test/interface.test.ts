import { describe, expect, test } from "bun:test"
import { z } from "zod"
import { RookeryError } from "../src/errors"
import { defineInterface, type Interface, isInterface, type ServiceImpl } from "../src/interface"

describe("@rookery/sdk defineInterface", () => {
  test("describes services and events, and freezes them", () => {
    const api = defineInterface({
      services: {
        greeter: {
          greet: {
            params: z.object({ name: z.string() }),
            result: z.object({ text: z.string() }),
          },
          watch: {
            stream: true,
            params: z.object({ id: z.string() }),
            result: z.object({ line: z.string() }),
          },
        },
      },
      events: { "greeter.greeted": z.object({ name: z.string() }) },
    })

    expect(api.__rookery).toBe("interface")
    expect(Object.keys(api.services)).toEqual(["greeter"])
    expect(Object.keys(api.services.greeter.methods)).toEqual(["greet", "watch"])
    expect(api.services.greeter.methods.watch.stream).toBe(true)
    expect(Object.keys(api.events)).toEqual(["greeter.greeted"])
    expect(Object.isFrozen(api)).toBe(true)
    expect(Object.isFrozen(api.services)).toBe(true)
    expect(Object.isFrozen(api.services.greeter.methods)).toBe(true)
    expect(isInterface(api)).toBe(true)
    expect(isInterface({ services: {} })).toBe(false)
    expect(isInterface(null)).toBe(false)
  })

  test("defaults services and events to empty objects", () => {
    const api = defineInterface({})
    expect(api.services).toEqual({})
    expect(api.events).toEqual({})
  })

  test("refuses a method called stream, which is how a dependent streams one", () => {
    expect(() =>
      defineInterface({
        services: {
          greeter: {
            stream: {
              params: z.object({}),
              result: z.object({}),
            },
          },
        },
      }),
    ).toThrow(RookeryError)
    expect(() =>
      defineInterface({
        services: {
          greeter: {
            stream: { params: z.object({}), result: z.object({}) },
          },
        },
      }),
    ).toThrow(/has a method called stream/)
  })

  test("types a provider from the declared methods", () => {
    const api = defineInterface({
      services: {
        greeter: {
          greet: {
            params: z.object({ name: z.string() }),
            result: z.string(),
          },
        },
      },
    })

    const impl: ServiceImpl<typeof api.services.greeter.methods> & {
      extra?: never
    } = {
      greet: (_ctx, { name }) => `Hello, ${name}`,
    }
    expect(impl.greet({} as never, { name: "Ada" })).toBe("Hello, Ada")
  })

  test("an interface itself is a valid value", () => {
    const api: Interface = defineInterface({})
    expect(api.__rookery).toBe("interface")
  })
})
