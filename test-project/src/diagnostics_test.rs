// This file contains intentional errors for testing diagnostics.

fn test_undefined_variable() {
    println!("{}", undefined_var); // Error: undefined variable
}

fn test_type_mismatch() -> i32 {
    "not a number" // Error: type mismatch
}

fn test_unused_variable() {
    let unused = 42; // Warning: unused variable
}

fn test_dead_code() {
    // Warning: function is never used
    println!("This function is never called");
}

fn test_missing_lifetime<'a>(x: &str, y: &'a str) -> &str {
    if x.len() > y.len() {
        x // Error: missing lifetime specifier
    } else {
        y
    }
}

fn test_moved_value() {
    let s = String::from("hello");
    let s2 = s;
    println!("{}", s); // Error: value moved
}

fn test_borrow_checker() {
    let mut v = vec![1, 2, 3];
    let first = &v[0];
    v.push(4); // Error: cannot borrow as mutable
    println!("{}", first);
}

#[allow(dead_code)]
struct UnusedStruct {
    // No warning due to allow attribute
    field: i32,
}

fn main() {
    test_unused_variable();
    // test_dead_code() is never called
}
