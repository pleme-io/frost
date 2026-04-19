
setopt norcs errreturn
fn() {
  if false; then
    print Bad
  else
    print Good
  fi
  print Better
}
fn
print In .zshenv
