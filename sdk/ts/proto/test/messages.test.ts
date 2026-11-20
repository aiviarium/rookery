import { describe, expect, test } from "bun:test";

import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  AuthService,
  Level,
  SpaceServiceListResponseSchema,
  SpaceSchema,
  UserSchema,
} from "@rookery/proto";

describe("the generated contract", () => {
  test("carries a message over the wire", () => {
    const user = create(UserSchema, {
      id: "01huser",
      email: "ada@example.test",
      name: "Ada",
      admin: true,
    });
    const decoded = fromBinary(UserSchema, toBinary(UserSchema, user));
    expect(decoded).toEqual(user);
  });

  test("names its procedures as the edge serves them", () => {
    expect(AuthService.typeName).toBe("rookery.v1.AuthService");
    expect(AuthService.method.me.name).toBe("Me");
  });

  test("carries a repeated message field", () => {
    const page = create(SpaceServiceListResponseSchema, {
      spaces: [
        create(SpaceSchema, { id: "01hspace", name: "Lab", kind: "notebook" }),
        create(SpaceSchema, { id: "01hother", name: "Desk", kind: "notebook" }),
      ],
    });
    const decoded = fromBinary(SpaceServiceListResponseSchema, toBinary(SpaceServiceListResponseSchema, page));
    expect(decoded.spaces.map((space) => space.name)).toEqual(["Lab", "Desk"]);
  });

  test("carries the levels the access option names", () => {
    expect(Level.VIEW).toBe(1);
    expect(Level.USE).toBe(2);
    expect(Level.MANAGE).toBe(3);
  });
});
