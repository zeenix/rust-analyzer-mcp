// Simple file with an obvious error for testing.

fn main() {
    let x: i32 = "not a number"; // Type error: expected i32, found &str
    println!("{}", undefined_variable); // Error: undefined variable
}
