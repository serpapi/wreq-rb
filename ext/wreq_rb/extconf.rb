# frozen_string_literal: true

require "mkmf"
require "rb_sys/mkmf"

# Apply patches to vendored dependencies before compilation
patch_dir = File.expand_path("../../patches", __dir__)
wreq_dir = File.expand_path("../../vendor/wreq", __dir__)

if File.directory?(patch_dir)
  Dir.glob(File.join(patch_dir, "*.patch")).sort.each do |patch|
    check = `cd #{wreq_dir} && git apply --check --reverse #{patch} 2>&1`
    if $?.success?
      puts "Patch already applied: #{File.basename(patch)}"
    else
      puts "Applying patch: #{File.basename(patch)}"
      system("cd #{wreq_dir} && git apply #{patch}") || abort("Failed to apply #{patch}")
    end
  end
end

create_rust_makefile("wreq_rb/wreq_rb")
