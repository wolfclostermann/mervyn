# State migration: resources were previously at the root module; they now live under
# module.oci_stack. Remove this file after a successful apply on upgraded Terraform.
moved {
  from = data.oci_identity_availability_domains.ads
  to   = module.oci_stack.data.oci_identity_availability_domains.ads
}

moved {
  from = data.oci_core_images.ubuntu_arm
  to   = module.oci_stack.data.oci_core_images.ubuntu_arm
}

moved {
  from = oci_core_vcn.this
  to   = module.oci_stack.oci_core_vcn.this
}

moved {
  from = oci_core_internet_gateway.this
  to   = module.oci_stack.oci_core_internet_gateway.this
}

moved {
  from = oci_core_default_route_table.public
  to   = module.oci_stack.oci_core_default_route_table.public
}

moved {
  from = oci_core_security_list.public
  to   = module.oci_stack.oci_core_security_list.public
}

moved {
  from = oci_core_subnet.public
  to   = module.oci_stack.oci_core_subnet.public
}

moved {
  from = oci_core_instance.mervyn
  to   = module.oci_stack.oci_core_instance.mervyn
}
