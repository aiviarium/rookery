// What a failed call tells a person.
import { Code, ConnectError } from "@connectrpc/connect";

/** errorMessage is a failure in words: the server's own sentence, capitalised, without the code. */
export function errorMessage(error: unknown): string {
  if (error instanceof ConnectError) {
    if (error.code === Code.Unauthenticated) return "Your session ended. Sign in again.";
    if (error.code === Code.Unavailable && error.rawMessage === "") return "The server could not be reached.";
    return sentence(error.rawMessage);
  }
  if (error instanceof Error) return sentence(error.message);
  return sentence(String(error));
}

function sentence(text: string): string {
  const trimmed = text.trim();
  if (trimmed === "") return "Something went wrong.";
  return trimmed[0]!.toUpperCase() + trimmed.slice(1);
}
