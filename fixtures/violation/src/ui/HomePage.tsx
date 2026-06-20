// Violation: presentation layer importing infrastructure.
import { UserRepository } from "../../data/infrastructure/UserRepository";

export function HomePage() {
  const repo = new UserRepository();
  return repo.loadCurrentUser();
}
