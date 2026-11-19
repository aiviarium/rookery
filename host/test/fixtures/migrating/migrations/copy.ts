import type { MigrationTx } from "@rookery/sdk"

export default async (tx: MigrationTx) => {
  for await (const row of tx.each("notes")) {
    await tx.insert("copies", { of: row.id, text: String(row.text).toUpperCase() })
  }
  tx.log("copied")
}
